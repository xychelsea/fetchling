//! HTTP/1.1 retrieval with TLS connector reuse and keep-alive pooling.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use base64::Engine as _;
use bytes::Bytes;
use fetchling_core::{
    dest_label, format_connected, format_dns, format_fetch_start, format_http_status,
    format_length_detail, format_redirect_hop, format_reuse, format_saving_as, strip_query_vars,
    Config, Error, Logger, ProgressBar, Result,
};
use fetchling_net::{
    absolute_http_request_target, build_connector, connect_happy_eyeballs, connect_to_proxy,
    connect_via_http_connect, proxy_basic_auth, proxy_bypassed, proxy_endpoint_key, proxy_url_for,
    read_timeout_dur, DnsCache, HstsStore, RateLimiter,
};
use http::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, IF_MODIFIED_SINCE, LOCATION, RANGE,
    USER_AGENT,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use httpdate::{fmt_http_date, parse_http_date};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use tokio::time::timeout;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;
use url::Url;

mod cookies;

pub use cookies::Jar;

const MAX_IDLE_PER_KEY: usize = 2;
const MAX_IDLE_TOTAL: usize = 32;
const WARC_TEE_CAP: usize = 64 * 1024 * 1024;

type PooledSender = SendRequest<Full<Bytes>>;

/// Idle connection key. Must include proxy endpoint so pooled senders are
/// never reused across hosts or between direct and proxied paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    scheme: String,
    host: String,
    port: u16,
    proxy: String,
}

#[derive(Default)]
struct IdlePool {
    map: HashMap<PoolKey, VecDeque<(PooledSender, Option<IpAddr>)>>,
    total: usize,
}

impl IdlePool {
    fn take(&mut self, key: &PoolKey) -> Option<(PooledSender, Option<IpAddr>)> {
        let q = self.map.get_mut(key)?;
        let s = q.pop_front()?;
        self.total = self.total.saturating_sub(1);
        if q.is_empty() {
            self.map.remove(key);
        }
        Some(s)
    }

    fn put(&mut self, key: PoolKey, sender: PooledSender, peer_ip: Option<IpAddr>) {
        if self.total >= MAX_IDLE_TOTAL {
            return;
        }
        let q = self.map.entry(key).or_default();
        if q.len() >= MAX_IDLE_PER_KEY {
            return;
        }
        q.push_back((sender, peer_ip));
        self.total += 1;
    }

    #[cfg(test)]
    fn total_idle(&self) -> usize {
        self.total
    }

    #[cfg(test)]
    fn idle_for(&self, key: &PoolKey) -> usize {
        self.map.get(key).map(|q| q.len()).unwrap_or(0)
    }
}

pub struct HttpClient {
    dns: DnsCache,
    tls: TlsConnector,
    jar: Mutex<Jar>,
    pool: Mutex<IdlePool>,
    hsts: Mutex<HstsStore>,
    log: Logger,
}

impl HttpClient {
    pub fn new(cfg: &Config, log: Logger) -> Result<Self> {
        let mut jar = Jar::new();
        if cfg.cookies {
            if let Some(path) = &cfg.load_cookies {
                jar = Jar::load(Path::new(path))?;
            }
        }
        let hsts = if cfg.hsts {
            cfg.hsts_file
                .as_ref()
                .map(|p| HstsStore::load(p))
                .unwrap_or_default()
        } else {
            HstsStore::default()
        };
        let tls = build_connector(cfg)?;
        Ok(Self {
            dns: DnsCache::new(),
            tls,
            jar: Mutex::new(jar),
            pool: Mutex::new(IdlePool::default()),
            hsts: Mutex::new(hsts),
            log,
        })
    }

    pub fn save_cookies(&self, path: &Path, keep_session: bool) -> Result<()> {
        let jar = self
            .jar
            .lock()
            .map_err(|_| Error::Message("cookie jar lock poisoned".into()))?;
        jar.save(path, keep_session)
    }

    pub fn save_hsts(&self, path: &str) -> Result<()> {
        let store = self
            .hsts
            .lock()
            .map_err(|_| Error::Message("hsts lock poisoned".into()))?;
        store.save(path)
    }

    pub async fn download(&self, cfg: &Config, url: &Url, dest: &Path) -> Result<FetchMeta> {
        let mut current = url.clone();
        if cfg.hsts && current.scheme() == "http" {
            if let Some(host) = current.host_str() {
                let store = self
                    .hsts
                    .lock()
                    .map_err(|_| Error::Message("hsts lock poisoned".into()))?;
                if store.should_upgrade(host) {
                    let _ = current.set_scheme("https");
                }
            }
        }

        self.log.narrative(&format_fetch_start(current.as_str()));

        let auth_origin = AuthOrigin::from_url(cfg, &current);

        let mut redirects = 0u32;
        loop {
            if cfg.https_only && current.scheme() != "https" {
                return Err(Error::Protocol(
                    "HTTPS-only mode: refusing non-HTTPS URL".into(),
                ));
            }

            let meta = self.fetch_once(cfg, &current, dest, &auth_origin).await?;
            if let Some(loc) = &meta.redirect_to {
                redirects += 1;
                if redirects > cfg.max_redirect {
                    return Err(Error::Protocol("too many redirections".into()));
                }
                current = current
                    .join(loc)
                    .map_err(|e| Error::Parse(format!("bad redirect: {e}")))?;
                self.log.narrative(&format_redirect_hop(current.as_str()));
                continue;
            }
            return Ok(meta);
        }
    }

    async fn fetch_once(
        &self,
        cfg: &Config,
        url: &Url,
        dest: &Path,
        auth_origin: &AuthOrigin,
    ) -> Result<FetchMeta> {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(Error::Protocol(format!("unsupported scheme: {scheme}")));
        }

        let host = url
            .host_str()
            .ok_or_else(|| Error::Parse("URL missing host".into()))?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| Error::Parse("URL missing port".into()))?;

        let http_proxy = if scheme == "http" {
            proxy_url_for(cfg, "http").filter(|_| !proxy_bypassed(&host))
        } else if scheme == "https" {
            proxy_url_for(cfg, "https").filter(|_| !proxy_bypassed(&host))
        } else {
            None
        };
        let proxy_key = proxy_endpoint_key(http_proxy.as_deref());

        let key = PoolKey {
            scheme: scheme.to_string(),
            host: host.clone(),
            port,
            proxy: proxy_key,
        };

        let mut from_pool = false;
        let mut peer_ip = None;
        let mut sender = if cfg.http_keep_alive {
            let taken = self
                .pool
                .lock()
                .map_err(|_| Error::Message("connection pool lock poisoned".into()))?
                .take(&key);
            from_pool = taken.is_some();
            if let Some((s, ip)) = taken {
                peer_ip = ip;
                Some(s)
            } else {
                None
            }
        } else {
            None
        };

        if from_pool {
            self.log.narrative(&format_reuse(&host, port));
        }

        if sender.is_none() {
            let (s, ip) = self
                .open_sender(cfg, scheme, &host, port, http_proxy.as_deref())
                .await?;
            peer_ip = ip;
            sender = Some(s);
        }

        let mut sender = sender.expect("sender just set");
        match self
            .exchange(
                cfg,
                url,
                dest,
                &host,
                port,
                auth_origin,
                &mut sender,
                http_proxy.as_deref(),
                peer_ip,
            )
            .await
        {
            Ok(meta) => {
                if cfg.http_keep_alive {
                    if let Ok(mut pool) = self.pool.lock() {
                        pool.put(key, sender, peer_ip);
                    }
                }
                Ok(meta)
            }
            Err(_e) if from_pool => {
                let (mut sender, peer_ip) = self
                    .open_sender(cfg, scheme, &host, port, http_proxy.as_deref())
                    .await?;
                let meta = self
                    .exchange(
                        cfg,
                        url,
                        dest,
                        &host,
                        port,
                        auth_origin,
                        &mut sender,
                        http_proxy.as_deref(),
                        peer_ip,
                    )
                    .await?;
                if cfg.http_keep_alive {
                    if let Ok(mut pool) = self.pool.lock() {
                        pool.put(key, sender, peer_ip);
                    }
                }
                Ok(meta)
            }
            Err(e) => Err(e),
        }
    }

    async fn open_sender(
        &self,
        cfg: &Config,
        scheme: &str,
        host: &str,
        port: u16,
        proxy_url: Option<&str>,
    ) -> Result<(PooledSender, Option<IpAddr>)> {
        let tcp = if scheme == "https" {
            if let Some(proxy) = proxy_url {
                self.log
                    .narrative(&format!("Connecting via proxy {proxy} for {host}:{port}"));
                connect_via_http_connect(cfg, &self.dns, proxy, host, port).await?
            } else {
                let addrs = self.dns.lookup(cfg, host, port).await?;
                self.log.narrative(&format_dns(host, &addrs));
                connect_happy_eyeballs(cfg, &addrs).await?
            }
        } else if let Some(proxy) = proxy_url {
            self.log
                .narrative(&format!("Connecting via proxy {proxy} for {host}:{port}"));
            connect_to_proxy(cfg, &self.dns, proxy).await?.0
        } else {
            let addrs = self.dns.lookup(cfg, host, port).await?;
            self.log.narrative(&format_dns(host, &addrs));
            connect_happy_eyeballs(cfg, &addrs).await?
        };
        let peer_ip = tcp.peer_addr().ok().map(|a| a.ip());
        if let Ok(peer) = tcp.peer_addr() {
            self.log.narrative(&format_connected(peer));
        }

        if scheme == "https" {
            let server_name = ServerName::try_from(host.to_string())
                .map_err(|e| Error::Tls(format!("server name: {e}")))?;
            let tls = self
                .tls
                .connect(server_name, tcp)
                .await
                .map_err(|e| Error::Tls(format!("handshake: {e}")))?;
            let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
                .await
                .map_err(|e| Error::Network(format!("HTTP handshake: {e}")))?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok((sender, peer_ip))
        } else {
            let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
                .await
                .map_err(|e| Error::Network(format!("HTTP handshake: {e}")))?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok((sender, peer_ip))
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        cfg: &Config,
        url: &Url,
        dest: &Path,
        host: &str,
        port: u16,
        auth_origin: &AuthOrigin,
        sender: &mut PooledSender,
        http_proxy: Option<&str>,
        peer_ip: Option<IpAddr>,
    ) -> Result<FetchMeta> {
        let method = if cfg.spider && cfg.method.is_none() && cfg.post_data.is_none() {
            Method::GET
        } else if let Some(m) = &cfg.method {
            Method::from_bytes(m.as_bytes()).unwrap_or(Method::GET)
        } else if cfg.post_data.is_some() || cfg.post_file.is_some() {
            Method::POST
        } else {
            Method::GET
        };

        let body_bytes = load_body(cfg)?;
        let request_url = strip_query_vars(url, cfg.cut_url_get_vars.as_deref());
        let use_absolute = url.scheme() == "http" && http_proxy.is_some();
        let path_q = if use_absolute {
            absolute_http_request_target(&request_url)
        } else {
            let mut p = request_url.path().to_string();
            if let Some(q) = request_url.query() {
                p.push('?');
                p.push_str(q);
            }
            if p.is_empty() {
                p.push('/');
            }
            p
        };

        let mut builder = Request::builder().method(method.clone()).uri(&path_q);
        let headers = builder
            .headers_mut()
            .ok_or_else(|| Error::Protocol("failed to build request headers".into()))?;
        let host_val =
            if (port == 80 && url.scheme() == "http") || (port == 443 && url.scheme() == "https") {
                host.to_string()
            } else {
                format!("{host}:{port}")
            };
        headers.insert(
            HOST,
            HeaderValue::from_str(&host_val)
                .map_err(|e| Error::Parse(format!("invalid Host header: {e}")))?,
        );
        if use_absolute {
            if let Some(proxy_str) = http_proxy {
                if let Ok(proxy) = url::Url::parse(proxy_str) {
                    if let Some(token) = proxy_basic_auth(cfg, &proxy) {
                        headers.insert(
                            HeaderName::from_static("proxy-authorization"),
                            HeaderValue::from_str(&format!("Basic {token}")).map_err(|e| {
                                Error::Parse(format!("invalid Proxy-Authorization: {e}"))
                            })?,
                        );
                    }
                }
            }
        }
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&cfg.user_agent).unwrap_or(HeaderValue::from_static("fetchling")),
        );
        if !cfg.cache {
            headers.insert(
                HeaderName::from_static("cache-control"),
                HeaderValue::from_static("no-cache"),
            );
            headers.insert(
                HeaderName::from_static("pragma"),
                HeaderValue::from_static("no-cache"),
            );
        }
        if let Some(r) = &cfg.referer {
            headers.insert(
                HeaderName::from_static("referer"),
                HeaderValue::from_str(r).unwrap_or(HeaderValue::from_static("")),
            );
        }
        if cfg.compression == "gzip" || cfg.compression == "auto" {
            headers.insert(
                HeaderName::from_static("accept-encoding"),
                HeaderValue::from_static("gzip"),
            );
        }
        for h in &cfg.headers {
            if let Some((k, v)) = h.split_once(':') {
                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.trim().as_bytes()),
                    HeaderValue::from_str(v.trim()),
                ) {
                    headers.insert(name, val);
                }
            }
        }
        if cfg.cookies {
            let cookie = self
                .jar
                .lock()
                .map_err(|_| Error::Message("cookie jar lock poisoned".into()))?
                .cookie_header(url);
            if let Some(c) = cookie {
                headers.insert(
                    COOKIE,
                    HeaderValue::from_str(&c).unwrap_or(HeaderValue::from_static("")),
                );
            }
        }
        maybe_auth(cfg, url, auth_origin, headers);

        let mut start_pos = cfg.start_pos.unwrap_or(0);
        if cfg.continue_download && cfg.start_pos.is_none() && dest.exists() {
            start_pos = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        }
        let resuming = start_pos > 0;
        if resuming {
            self.log.info(&format!("continuing at byte {start_pos}"));
            headers.insert(
                RANGE,
                HeaderValue::from_str(&format!("bytes={start_pos}-"))
                    .map_err(|e| Error::Parse(format!("invalid Range header: {e}")))?,
            );
        } else if cfg.timestamping
            && cfg.if_modified_since
            && !cfg.spider
            && dest.as_os_str() != "-"
            && dest.exists()
        {
            if let Some(mtime) = local_mtime(dest) {
                if let Ok(v) = HeaderValue::from_str(&fmt_http_date(mtime)) {
                    headers.insert(IF_MODIFIED_SINCE, v);
                }
            }
        }

        if method == Method::POST || method == Method::PUT || !body_bytes.is_empty() {
            headers.insert(CONTENT_LENGTH, HeaderValue::from(body_bytes.len()));
            if !headers.contains_key(CONTENT_TYPE) && (cfg.post_data.is_some()) {
                headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
        }

        let warc_request = if cfg.warc_file.is_some() {
            Some(format_request_bytes(&method, &path_q, headers, &body_bytes))
        } else {
            None
        };

        let req = builder
            .body(Full::new(Bytes::from(body_bytes)))
            .map_err(|e| Error::Protocol(format!("build request: {e}")))?;

        let response = sender
            .send_request(req)
            .await
            .map_err(|e| Error::Network(format!("request failed: {e}")))?;

        let status = response.status();
        let mut warc_response = if cfg.warc_file.is_some() {
            Some(format_response_headers(status, response.headers()))
        } else {
            None
        };
        self.log.narrative(&format_http_status(
            status.as_u16(),
            status.canonical_reason(),
        ));
        self.log.server(&format!(
            "HTTP/1.1 {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        ));
        for (k, v) in response.headers().iter() {
            self.log
                .server(&format!("{k}: {}", v.to_str().unwrap_or("")));
        }

        if cfg.hsts && url.scheme() == "https" {
            if let Some(host) = url.host_str() {
                if let Some(sts) = response
                    .headers()
                    .get(http::header::STRICT_TRANSPORT_SECURITY)
                    .and_then(|v| v.to_str().ok())
                {
                    if let Ok(mut store) = self.hsts.lock() {
                        store.learn(host, sts);
                    }
                }
            }
        }

        if cfg.cookies {
            let set_cookies: Vec<_> = response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .collect();
            self.jar
                .lock()
                .map_err(|_| Error::Message("cookie jar lock poisoned".into()))?
                .store_from_headers(url, set_cookies.iter().map(|s| s.as_str()));
        }

        let link_headers: Vec<String> = response
            .headers()
            .get_all(http::header::LINK)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .collect();

        if status.is_redirection() {
            let loc = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let _ = response.collect().await;
            let mut meta = FetchMeta::basic(status.as_u16(), url);
            meta.redirect_to = loc;
            meta.warc_request = warc_request;
            meta.warc_response = warc_response;
            meta.link_headers = link_headers;
            meta.peer_ip = peer_ip;
            return Ok(meta);
        }

        if status == StatusCode::NOT_MODIFIED {
            let _ = response.collect().await;
            self.log
                .info("HTTP request sent, awaiting response... 304 Not Modified");
            let mut meta = FetchMeta::basic(304, url);
            meta.warc_request = warc_request;
            meta.warc_response = warc_response;
            meta.link_headers = link_headers;
            meta.peer_ip = peer_ip;
            return Ok(meta);
        }

        if status == StatusCode::RANGE_NOT_SATISFIABLE && start_pos > 0 && dest.exists() {
            let _ = response.collect().await;
            self.log
                .info("range not satisfiable; local file already complete");
            let mut meta = FetchMeta::basic(status.as_u16(), url);
            meta.warc_request = warc_request;
            meta.warc_response = warc_response;
            meta.link_headers = link_headers;
            meta.peer_ip = peer_ip;
            return Ok(meta);
        }

        if start_pos > 0 && status == StatusCode::OK {
            self.log.info("cannot resume; re-getting from scratch");
        }

        let retryable = cfg.retry_on_http_error.contains(&status.as_u16());
        if status.is_client_error() || status.is_server_error() {
            if !cfg.content_on_error && !retryable {
                let _ = response.collect().await;
                return Err(Error::Server(format!(
                    "server returned status {}",
                    status.as_u16()
                )));
            }
            if retryable {
                let _ = response.collect().await;
                return Err(Error::Server(format!("retryable HTTP {}", status.as_u16())));
            }
        }

        let last_modified = response
            .headers()
            .get(http::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| parse_http_date(s).ok());

        if cfg.timestamping
            && !cfg.if_modified_since
            && !cfg.spider
            && !resuming
            && dest.as_os_str() != "-"
            && dest.exists()
        {
            if let (Some(server_mtime), Some(local)) = (last_modified, local_mtime(dest)) {
                if server_mtime <= local {
                    let _ = response.collect().await;
                    self.log.info("remote file not newer; not retrieving");
                    let mut meta = FetchMeta::basic(304, url);
                    meta.warc_request = warc_request;
                    meta.warc_response = warc_response;
                    meta.link_headers = link_headers;
                    meta.peer_ip = peer_ip;
                    return Ok(meta);
                }
            }
        }

        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_disposition_filename = if cfg.content_disposition {
            response
                .headers()
                .get(http::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_disposition_filename)
        } else {
            None
        };
        let content_encoding = response
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let len = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());
        let content_range = response
            .headers()
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if cfg.spider {
            let _ = response.collect().await;
            return Ok(FetchMeta {
                status: status.as_u16(),
                redirect_to: None,
                content_type,
                content_disposition_filename,
                bytes_written: 0,
                final_url: url.clone(),
                warc_request,
                warc_response,
                link_headers,
                peer_ip,
            });
        }

        let append = start_pos > 0 && status == StatusCode::PARTIAL_CONTENT;
        let progress_total = if append {
            resume_progress_total(start_pos, len, content_range.as_deref())
        } else {
            len
        };
        let already = if append { Some(start_pos) } else { None };
        if let Some(line) =
            format_length_detail(progress_total.or(len), already, content_type.as_deref())
        {
            self.log.narrative(&line);
        }
        self.log.narrative(&format_saving_as(&dest_label(dest)));

        let header_prefix = if cfg.save_headers && !append {
            Some(format_response_headers(status, response.headers()))
        } else {
            None
        };

        let body = response.into_body();
        let tee_warc = cfg.warc_file.is_some();
        let (bytes_written, warc_body) = write_body(
            cfg,
            dest,
            body,
            start_pos,
            status,
            progress_total,
            content_encoding.as_deref(),
            header_prefix.as_deref(),
            tee_warc,
        )
        .await?;

        if let (Some(wr), Some(body_bytes)) = (warc_response.as_mut(), warc_body) {
            wr.extend_from_slice(&body_bytes);
        }

        if !cfg.ignore_length {
            if let Some(expected) = len {
                let gzip = content_encoding
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case("gzip"));
                if let Some(msg) = content_length_mismatch(expected, bytes_written, gzip) {
                    return Err(Error::Protocol(msg));
                }
            }
        }

        if cfg.use_server_timestamps {
            if let Some(mtime) = last_modified {
                set_local_mtime(dest, mtime)?;
            }
        }

        Ok(FetchMeta {
            status: status.as_u16(),
            redirect_to: None,
            content_type,
            content_disposition_filename,
            bytes_written,
            final_url: url.clone(),
            warc_request,
            warc_response,
            link_headers,
            peer_ip,
        })
    }
}

#[derive(Debug)]
pub struct FetchMeta {
    pub status: u16,
    pub redirect_to: Option<String>,
    pub content_type: Option<String>,
    pub content_disposition_filename: Option<String>,
    pub bytes_written: u64,
    pub final_url: Url,
    pub warc_request: Option<Vec<u8>>,
    pub warc_response: Option<Vec<u8>>,
    pub link_headers: Vec<String>,
    pub peer_ip: Option<IpAddr>,
}

impl FetchMeta {
    fn basic(status: u16, url: &Url) -> Self {
        Self {
            status,
            redirect_to: None,
            content_type: None,
            content_disposition_filename: None,
            bytes_written: 0,
            final_url: url.clone(),
            warc_request: None,
            warc_response: None,
            link_headers: Vec::new(),
            peer_ip: None,
        }
    }
}

fn load_body(cfg: &Config) -> Result<Vec<u8>> {
    if let Some(data) = &cfg.body_data {
        return Ok(data.as_bytes().to_vec());
    }
    if let Some(path) = &cfg.body_file {
        return Ok(std::fs::read(path)?);
    }
    if let Some(data) = &cfg.post_data {
        return Ok(data.as_bytes().to_vec());
    }
    if let Some(path) = &cfg.post_file {
        return Ok(std::fs::read(path)?);
    }
    Ok(Vec::new())
}

struct AuthOrigin {
    host: Option<String>,
    scheme: String,
    user: Option<String>,
    pass: Option<String>,
}

impl AuthOrigin {
    fn from_url(cfg: &Config, url: &Url) -> Self {
        let mut user = cfg
            .http_user
            .as_ref()
            .or(cfg.user.as_ref())
            .cloned()
            .or_else(|| {
                if !url.username().is_empty() {
                    Some(url.username().to_string())
                } else {
                    None
                }
            });
        let mut pass = cfg
            .http_password
            .as_ref()
            .or(cfg.password.as_ref())
            .cloned()
            .or_else(|| url.password().map(|p| p.to_string()));

        if cfg.netrc && (user.is_none() || pass.is_none()) {
            if let Some(host) = url.host_str() {
                if let Some((nuser, npass)) =
                    fetchling_core::lookup_credentials(host, cfg.netrc_file.as_deref())
                {
                    if user.is_none() {
                        user = Some(nuser);
                    }
                    if pass.is_none() {
                        pass = Some(npass);
                    }
                }
            }
        }

        Self {
            host: url.host_str().map(|s| s.to_string()),
            scheme: url.scheme().to_string(),
            user,
            pass,
        }
    }

    fn matches(&self, url: &Url) -> bool {
        url.scheme() == self.scheme && url.host_str() == self.host.as_deref()
    }
}

fn maybe_auth(cfg: &Config, url: &Url, auth_origin: &AuthOrigin, headers: &mut HeaderMap) {
    if !auth_origin.matches(url) {
        return;
    }

    if let Some(u) = &auth_origin.user {
        if cfg.auth_no_challenge || auth_origin.pass.is_some() {
            let token = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                u,
                auth_origin.pass.as_deref().unwrap_or("")
            ));
            if let Ok(v) = HeaderValue::from_str(&format!("Basic {token}")) {
                headers.insert(AUTHORIZATION, v);
            }
        }
    }
}

fn local_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn set_local_mtime(path: &Path, mtime: SystemTime) -> Result<()> {
    if path.as_os_str() == "-" || !path.exists() {
        return Ok(());
    }
    let file = std::fs::File::options().write(true).open(path)?;
    let times = std::fs::FileTimes::new().set_modified(mtime);
    file.set_times(times)?;
    Ok(())
}

fn format_response_headers(status: StatusCode, headers: &HeaderMap) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .into_bytes();
    for (k, v) in headers.iter() {
        out.extend_from_slice(k.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

fn format_request_bytes(
    method: &Method,
    path_q: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!("{method} {path_q} HTTP/1.1\r\n").into_bytes();
    for (k, v) in headers.iter() {
        out.extend_from_slice(k.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Extract a basename from Content-Disposition (`filename` / `filename*`).
pub fn parse_content_disposition_filename(header: &str) -> Option<String> {
    let mut filename = None;
    for part in header.split(';') {
        let part = part.trim();
        let lower = part.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("filename*=") {
            let raw = part[part.len() - rest.len()..].trim();
            let raw = raw.trim_matches('"');
            // charset'lang'value
            if let Some(encoded) = raw.splitn(3, '\'').nth(2) {
                let decoded = percent_encoding::percent_decode_str(encoded)
                    .decode_utf8()
                    .ok()?
                    .into_owned();
                filename = Some(decoded);
            }
        } else if let Some(rest) = lower.strip_prefix("filename=") {
            let raw = part[part.len() - rest.len()..].trim();
            let raw = raw.trim_matches('"');
            filename = Some(raw.to_string());
        }
    }
    let name = filename?;
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&name)
        .trim()
        .to_string();
    if base.is_empty() || base == "." || base == ".." {
        None
    } else {
        Some(base)
    }
}

async fn next_frame(
    body: &mut Incoming,
    read_to: Option<std::time::Duration>,
) -> Result<Option<hyper::body::Frame<Bytes>>> {
    let fut = body.frame();
    let frame = if let Some(t) = read_to {
        match timeout(t, fut).await {
            Ok(r) => r,
            Err(_) => return Err(Error::Network("read timeout".into())),
        }
    } else {
        fut.await
    };
    match frame {
        None => Ok(None),
        Some(Ok(f)) => Ok(Some(f)),
        Some(Err(e)) => Err(Error::Network(format!("body: {e}"))),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_body(
    cfg: &Config,
    dest: &Path,
    mut body: Incoming,
    start_pos: u64,
    status: StatusCode,
    total: Option<u64>,
    content_encoding: Option<&str>,
    header_prefix: Option<&[u8]>,
    tee_warc: bool,
) -> Result<(u64, Option<Vec<u8>>)> {
    let to_stdout = dest.as_os_str() == "-";
    let append = start_pos > 0 && status == StatusCode::PARTIAL_CONTENT;
    let initial = if append { start_pos } else { 0 };
    let read_to = read_timeout_dur(cfg);

    let mut file = if to_stdout {
        None
    } else {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let f = if append {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dest)?
        } else {
            if cfg.unlink && dest.exists() {
                let _ = std::fs::remove_file(dest);
            }
            std::fs::File::create(dest)?
        };
        Some(f)
    };

    if let Some(prefix) = header_prefix {
        if let Some(f) = file.as_mut() {
            f.write_all(prefix)?;
        } else {
            std::io::stdout().write_all(prefix)?;
        }
    }

    let progress = ProgressBar::with_initial(cfg, total, initial, dest_label(dest));
    let limiter = RateLimiter::new(cfg.limit_rate);
    let gzip = content_encoding == Some("gzip");
    let gzip_cap = gzip_uncompressed_cap(total);

    if gzip {
        return write_body_gzip(
            body,
            file,
            progress,
            limiter,
            gzip_cap,
            read_to,
            tee_warc,
            cfg.warc_tempdir.as_deref(),
        )
        .await;
    }

    let mut written = 0u64;
    let mut progress = progress;
    let mut limiter = limiter;
    let mut tee = if tee_warc {
        Some(WarcTee::new(cfg.warc_tempdir.as_deref())?)
    } else {
        None
    };
    while let Some(frame) = next_frame(&mut body, read_to).await? {
        if let Some(data) = frame.data_ref() {
            if let Some(lim) = limiter.as_mut() {
                lim.take(data.len() as u64).await;
            }
            written += data.len() as u64;
            progress.update(data.len() as u64);
            if let Some(t) = tee.as_mut() {
                t.push(data);
            }
            if let Some(f) = file.as_mut() {
                f.write_all(data)?;
            } else {
                std::io::stdout().write_all(data)?;
            }
        }
    }
    progress.finish();
    Ok((written, tee.and_then(|t| t.finish())))
}

struct WarcTee {
    mem: Option<Vec<u8>>,
    file: Option<(std::fs::File, std::path::PathBuf)>,
    overflowed: bool,
}

impl WarcTee {
    fn new(tempdir: Option<&str>) -> Result<Self> {
        if let Some(dir) = tempdir {
            std::fs::create_dir_all(dir)?;
            let path = std::path::PathBuf::from(dir).join(format!(
                "fetchling-warc-tee-{}-{}.tmp",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let file = std::fs::File::create(&path)?;
            Ok(Self {
                mem: None,
                file: Some((file, path)),
                overflowed: false,
            })
        } else {
            Ok(Self {
                mem: Some(Vec::new()),
                file: None,
                overflowed: false,
            })
        }
    }

    fn push(&mut self, data: &[u8]) {
        if self.overflowed {
            return;
        }
        if let Some((f, _)) = self.file.as_mut() {
            if f.write_all(data).is_err() {
                self.overflowed = true;
            }
            return;
        }
        if let Some(buf) = self.mem.as_mut() {
            if buf.len().saturating_add(data.len()) > WARC_TEE_CAP {
                self.overflowed = true;
                buf.clear();
                eprintln!(
                    "fetchling: warning: WARC response body exceeds {WARC_TEE_CAP} bytes; writing headers only"
                );
                return;
            }
            buf.extend_from_slice(data);
        }
    }

    fn finish(self) -> Option<Vec<u8>> {
        if self.overflowed {
            if let Some((_, path)) = self.file {
                let _ = std::fs::remove_file(path);
            }
            return None;
        }
        if let Some((_, path)) = self.file {
            let data = std::fs::read(&path).ok();
            let _ = std::fs::remove_file(&path);
            return data;
        }
        self.mem
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_body_gzip(
    mut body: Incoming,
    mut file: Option<std::fs::File>,
    mut progress: ProgressBar,
    mut limiter: Option<RateLimiter>,
    gzip_cap: u64,
    read_to: Option<std::time::Duration>,
    tee_warc: bool,
    warc_tempdir: Option<&str>,
) -> Result<(u64, Option<Vec<u8>>)> {
    use flate2::write::GzDecoder;

    struct CapWriter<'a> {
        file: &'a mut Option<std::fs::File>,
        written: u64,
        cap: u64,
    }

    impl Write for CapWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written + buf.len() as u64 > self.cap {
                return Err(std::io::Error::other("gzip too large"));
            }
            if let Some(f) = self.file.as_mut() {
                f.write_all(buf)?;
            } else {
                std::io::stdout().write_all(buf)?;
            }
            self.written += buf.len() as u64;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if let Some(f) = self.file.as_mut() {
                f.flush()?;
            } else {
                std::io::stdout().flush()?;
            }
            Ok(())
        }
    }

    let mut tee = if tee_warc {
        Some(WarcTee::new(warc_tempdir)?)
    } else {
        None
    };
    let mut sink = CapWriter {
        file: &mut file,
        written: 0,
        cap: gzip_cap,
    };
    {
        let mut decoder = GzDecoder::new(&mut sink);
        while let Some(frame) = next_frame(&mut body, read_to).await? {
            if let Some(data) = frame.data_ref() {
                if let Some(lim) = limiter.as_mut() {
                    lim.take(data.len() as u64).await;
                }
                progress.update(data.len() as u64);
                if let Some(t) = tee.as_mut() {
                    t.push(data);
                }
                decoder.write_all(data).map_err(|e| {
                    if e.to_string().contains("gzip too large") {
                        Error::Protocol("gzip too large".into())
                    } else {
                        Error::Protocol(format!("gzip: {e}"))
                    }
                })?;
            }
        }
        decoder.try_finish().map_err(|e| {
            if e.to_string().contains("gzip too large") {
                Error::Protocol("gzip too large".into())
            } else {
                Error::Protocol(format!("gzip: {e}"))
            }
        })?;
    }
    progress.finish();
    Ok((sink.written, tee.and_then(|t| t.finish())))
}

/// Uncompressed gzip budget: at least 256 MiB, or Content-Length * 32 when known.
fn gzip_uncompressed_cap(content_length: Option<u64>) -> u64 {
    const MIN: u64 = 256 * 1024 * 1024;
    match content_length {
        Some(n) => MIN.max(n.saturating_mul(32)),
        None => MIN,
    }
}

fn content_length_mismatch(expected: u64, written: u64, gzip: bool) -> Option<String> {
    if gzip || written == expected {
        None
    } else {
        Some(format!(
            "Content-Length mismatch: expected {expected} bytes, got {written}"
        ))
    }
}

/// Parse `Content-Range: bytes start-end/total` and return `total` when finite.
pub fn parse_content_range_total(header: &str) -> Option<u64> {
    let s = header.trim();
    let rest = s.strip_prefix("bytes ")?.trim();
    let (_range, total) = rest.split_once('/')?;
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

/// Full entity size for resume progress: Content-Range total, else start + Content-Length.
pub fn resume_progress_total(
    start_pos: u64,
    content_length: Option<u64>,
    content_range: Option<&str>,
) -> Option<u64> {
    if let Some(h) = content_range {
        if let Some(t) = parse_content_range_total(h) {
            return Some(t);
        }
    }
    content_length.map(|n| start_pos.saturating_add(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_mismatch_skips_gzip_and_exact() {
        assert!(content_length_mismatch(10, 10, false).is_none());
        assert!(content_length_mismatch(10, 9, true).is_none());
        let msg = content_length_mismatch(10, 9, false).unwrap();
        assert!(msg.contains("expected 10"));
        assert!(msg.contains("got 9"));
    }

    #[test]
    fn idle_pool_respects_caps() {
        let mut pool = IdlePool::default();
        let key = PoolKey {
            scheme: "http".into(),
            host: "example.com".into(),
            port: 80,
            proxy: String::new(),
        };
        assert_eq!(pool.idle_for(&key), 0);
        assert_eq!(pool.total_idle(), 0);
        assert!(pool.take(&key).is_none());
    }

    #[test]
    fn gzip_cap_scales_with_content_length() {
        assert_eq!(gzip_uncompressed_cap(None), 256 * 1024 * 1024);
        assert_eq!(gzip_uncompressed_cap(Some(1024)), 256 * 1024 * 1024);
        let big = 20 * 1024 * 1024u64;
        assert_eq!(gzip_uncompressed_cap(Some(big)), big * 32);
    }

    #[test]
    fn parse_content_range_total_values() {
        assert_eq!(parse_content_range_total("bytes 0-499/1234"), Some(1234));
        assert_eq!(parse_content_range_total("bytes 100-199/1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes 0-499/*"), None);
        assert_eq!(parse_content_range_total("invalid"), None);
    }

    #[test]
    fn resume_progress_total_prefers_content_range() {
        assert_eq!(
            resume_progress_total(100, Some(50), Some("bytes 100-149/500")),
            Some(500)
        );
        assert_eq!(resume_progress_total(100, Some(50), None), Some(150));
        assert_eq!(resume_progress_total(100, None, None), None);
        assert_eq!(
            resume_progress_total(100, Some(50), Some("bytes 100-149/*")),
            Some(150)
        );
    }

    #[test]
    fn save_headers_prefix_includes_status_and_blank_line() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        let prefix = format_response_headers(StatusCode::OK, &headers);
        let s = String::from_utf8(prefix).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 "));
        assert!(s.contains("content-type: text/plain\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn set_and_read_local_mtime_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fetchling-mtime-{}", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        set_local_mtime(&path, target).unwrap();
        let got = local_mtime(&path).unwrap();
        let diff = got
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .abs_diff(1_700_000_000);
        assert!(diff <= 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_content_disposition_filename_basic() {
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename="report.pdf""#).as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            parse_content_disposition_filename(r#"inline; filename=foo.txt"#).as_deref(),
            Some("foo.txt")
        );
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename*=UTF-8''hello%20world.bin"#)
                .as_deref(),
            Some("hello world.bin")
        );
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename="../etc/passwd""#)
                .as_deref(),
            Some("passwd")
        );
    }
}
