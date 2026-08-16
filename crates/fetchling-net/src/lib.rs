//! DNS, TCP, TLS, HTTP-proxy, and rate-limiting primitives for a retriever.
//!
//! # What this crate is (and is not)
//!
//! IS: DNS ([`DnsCache`]), TCP connect and Happy Eyeballs
//! ([`connect_tcp`], [`connect_happy_eyeballs`]), rustls connector builders
//! ([`build_connector`], [`build_connector_resumable`]), HTTP CONNECT and
//! absolute-form proxy helpers, a token-bucket [`RateLimiter`], and a small
//! [`HstsStore`]. The public API is flat: items are re-exported at the crate
//! root. Drive behavior with [`fetchling_core::Config`] fields set directly;
//! CLI / wget names in those field docs are compatibility aliases. Async entry
//! points need a Tokio runtime.
//!
//! IS NOT: HTTP request/response framing, FTP, connection pooling, recursive
//! retrieval, or SOCKS—those live in other `fetchling-*` crates or your own
//! protocol code. This crate does not re-export [`Config`]
//! or [`Error`].
//!
//! # Typical integration
//!
//! 1. Start from [`Config::default`] and set net
//!    fields (`dns_servers`, timeouts, `limit_rate`, TLS, proxies)
//! 2. Create [`DnsCache::new`] and call [`DnsCache::lookup`]
//! 3. Connect with [`connect_happy_eyeballs`] / [`connect_tcp`], or a proxy path
//!    ([`proxy_url_for`] → [`proxy_bypassed`] → [`connect_to_proxy`] or
//!    [`connect_via_http_connect`])
//! 4. Call [`build_connector`] and handshake with the returned
//!    [`tokio_rustls::TlsConnector`]
//! 5. Wrap body reads with [`RateLimiter::new`] (`cfg.limit_rate`); optionally
//!    use [`HstsStore`] for HTTP→HTTPS upgrades
//!
//! # Areas
//!
//! - **DNS** — [`DnsCache`]
//! - **TCP** — [`connect_tcp`], [`connect_happy_eyeballs`], [`read_timeout_dur`]
//! - **TLS** — [`build_connector`], [`build_connector_resumable`]
//! - **Proxy** — [`proxy_url_for`], [`proxy_endpoint_key`], [`connect_to_proxy`],
//!   [`absolute_http_request_target`], [`proxy_bypassed`],
//!   [`host_matches_no_proxy`], [`format_http_connect_request`],
//!   [`proxy_basic_auth`], [`connect_via_http_connect`]
//! - **Rate limit** — [`RateLimiter`]
//! - **HSTS** — [`HstsStore`]
//!
//! # Examples
//!
//! Rate limiter, proxy helpers, and HSTS (no network):
//!
//! ```
//! use fetchling_core::Config;
//! use fetchling_net::{
//!     format_http_connect_request, host_matches_no_proxy, proxy_endpoint_key,
//!     HstsStore, RateLimiter,
//! };
//!
//! let mut cfg = Config::default();
//! cfg.limit_rate = Some(64 * 1024);
//! assert!(RateLimiter::new(cfg.limit_rate).is_some());
//!
//! assert_eq!(
//!     proxy_endpoint_key(Some("http://proxy.example:8080/")),
//!     "http://proxy.example:8080"
//! );
//! assert!(host_matches_no_proxy("a.example.com", "example.com"));
//!
//! let req = format_http_connect_request("example.com", 443, None);
//! assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
//!
//! let mut hsts = HstsStore::default();
//! hsts.learn("example.com", "max-age=31536000; includeSubDomains");
//! assert!(hsts.should_upgrade("example.com"));
//! assert!(hsts.should_upgrade("www.example.com"));
//! ```
//!
//! Build a rustls connector from default config:
//!
//! ```
//! use fetchling_core::Config;
//! use fetchling_net::build_connector;
//!
//! let connector = build_connector(&Config::default()).unwrap();
//! let _ = connector;
//! ```
//!
//! Resolve, connect, and prepare TLS (does not run; needs a network):
//!
//! ```no_run
//! use fetchling_core::Config;
//! use fetchling_net::{build_connector, connect_happy_eyeballs, DnsCache};
//!
//! # #[tokio::main]
//! # async fn main() {
//! let cfg = Config::default();
//! let dns = DnsCache::new();
//! let addrs = dns.lookup(&cfg, "example.com", 443).await.unwrap();
//! let _stream = connect_happy_eyeballs(&cfg, &addrs).await.unwrap();
//! let _tls = build_connector(&cfg).unwrap();
//! # }
//! ```

#![warn(missing_docs)]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fetchling_core::{Config, Error, Result};
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::timeout;

mod tls;

pub use tls::{build_connector, build_connector_resumable, HstsStore};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

/// In-process DNS cache keyed by `host:port`.
///
/// Lookups use the system resolver unless [`fetchling_core::Config::dns_servers`]
/// or [`fetchling_core::Config::bind_dns_address`] is set, in which case Hickory
/// is used. Results are filtered or sorted by `inet4_only`, `inet6_only`, and
/// `prefer_family`. Caching is honored only when
/// [`fetchling_core::Config::dns_cache`] is true (the default).
#[derive(Debug, Clone, Default)]
pub struct DnsCache {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<SocketAddr>>>>,
}

impl DnsCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `host`:`port` to socket addresses, optionally using the cache.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Network`] when lookup
    /// fails, times out, or yields no addresses. Returns
    /// [`Error::Parse`] for a bad `dns_servers` or
    /// `bind_dns_address` value.
    pub async fn lookup(&self, cfg: &Config, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
        let key = format!("{host}:{port}");
        if cfg.dns_cache {
            if let Ok(guard) = self.inner.lock() {
                if let Some(v) = guard.get(&key) {
                    return Ok(v.clone());
                }
            }
        }

        let dns_timeout = cfg.dns_timeout.or(cfg.timeout).map(Duration::from_secs_f64);

        let host_owned = host.to_string();
        let use_custom = cfg.dns_servers.is_some() || cfg.bind_dns_address.is_some();
        let lookup = async {
            if use_custom {
                lookup_custom(cfg, &host_owned, port).await
            } else {
                tokio::net::lookup_host((host_owned.as_str(), port))
                    .await
                    .map_err(|e| {
                        Error::Network(format!("DNS lookup failed for {host}:{port}: {e}"))
                    })
                    .map(|i| i.collect::<Vec<_>>())
            }
        };

        let mut addrs: Vec<SocketAddr> = if let Some(t) = dns_timeout {
            timeout(t, lookup)
                .await
                .map_err(|_| Error::Network(format!("DNS timeout for {host}")))??
        } else {
            lookup.await?
        };

        if cfg.inet4_only {
            addrs.retain(|a| a.is_ipv4());
        } else if cfg.inet6_only {
            addrs.retain(|a| a.is_ipv6());
        } else if cfg.prefer_family.eq_ignore_ascii_case("IPv4") {
            addrs.sort_by_key(|a| !a.is_ipv4());
        } else if cfg.prefer_family.eq_ignore_ascii_case("IPv6") {
            addrs.sort_by_key(|a| !a.is_ipv6());
        }

        if addrs.is_empty() {
            return Err(Error::Network(format!("no addresses for {host}:{port}")));
        }

        if cfg.dns_cache {
            if let Ok(mut guard) = self.inner.lock() {
                guard.insert(key, addrs.clone());
            }
        }
        Ok(addrs)
    }
}

async fn lookup_custom(cfg: &Config, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let resolver = build_custom_resolver(cfg)?;
    let response = resolver
        .lookup_ip(host)
        .await
        .map_err(|e| Error::Network(format!("DNS lookup failed for {host}:{port}: {e}")))?;
    Ok(response
        .iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect())
}

fn build_custom_resolver(cfg: &Config) -> Result<hickory_resolver::TokioResolver> {
    use hickory_resolver::config::{LookupIpStrategy, ResolverConfig, ResolverOpts};
    use hickory_resolver::net::runtime::TokioRuntimeProvider;
    use hickory_resolver::Resolver;

    let bind_addr = if let Some(bind) = &cfg.bind_dns_address {
        let ip: IpAddr = bind
            .parse()
            .map_err(|e| Error::Parse(format!("bad --bind-dns-address {bind}: {e}")))?;
        Some(SocketAddr::new(ip, 0))
    } else {
        None
    };

    let config = if let Some(servers) = &cfg.dns_servers {
        let mut name_servers = Vec::new();
        for part in servers.split(|c: char| c == ',' || c.is_whitespace()) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let addr = parse_nameserver(part)?;
            name_servers.push(nameserver_config(addr, bind_addr));
        }
        if name_servers.is_empty() {
            return Err(Error::Parse("empty --dns-servers".into()));
        }
        ResolverConfig::from_parts(None, vec![], name_servers)
    } else {
        let mut config = ResolverConfig::default();
        if bind_addr.is_some() {
            let name_servers: Vec<_> = config
                .name_servers()
                .iter()
                .cloned()
                .map(|mut ns| {
                    for conn in &mut ns.connections {
                        conn.bind_addr = bind_addr;
                    }
                    ns
                })
                .collect();
            config = ResolverConfig::from_parts(
                config.domain().cloned(),
                config.search().to_vec(),
                name_servers,
            );
        }
        config
    };

    let mut opts = ResolverOpts::default();
    opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;

    Resolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(opts)
        .build()
        .map_err(|e| Error::Network(format!("DNS resolver: {e}")))
}

fn nameserver_config(
    addr: SocketAddr,
    bind_addr: Option<SocketAddr>,
) -> hickory_resolver::config::NameServerConfig {
    use hickory_resolver::config::{ConnectionConfig, NameServerConfig};
    let mut udp = ConnectionConfig::udp();
    udp.port = addr.port();
    udp.bind_addr = bind_addr;
    let mut tcp = ConnectionConfig::tcp();
    tcp.port = addr.port();
    tcp.bind_addr = bind_addr;
    NameServerConfig::new(addr.ip(), true, vec![udp, tcp])
}

fn parse_nameserver(s: &str) -> Result<SocketAddr> {
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 53));
    }
    Err(Error::Parse(format!(
        "bad --dns-servers entry '{s}' (expected IP or IP:port)"
    )))
}

/// Resolve the I/O timeout from [`fetchling_core::Config`].
///
/// Uses `read_timeout` if set, otherwise `timeout`. Returns `None` when both
/// are unset or the chosen value is `<= 0`.
pub fn read_timeout_dur(cfg: &Config) -> Option<Duration> {
    cfg.read_timeout
        .or(cfg.timeout)
        .filter(|s| *s > 0.0)
        .map(Duration::from_secs_f64)
}

/// Open a TCP connection to `addr`.
///
/// Honors optional `bind_address`, `connect_timeout` (falling back to
/// `timeout`), and sets `TCP_NODELAY` on success.
///
/// # Errors
///
/// Returns [`Error::Network`] on bind, connect,
/// or timeout failure.
pub async fn connect_tcp(cfg: &Config, addr: SocketAddr) -> Result<TcpStream> {
    let connect_timeout = cfg
        .connect_timeout
        .or(cfg.timeout)
        .map(Duration::from_secs_f64);

    let fut = async {
        if let Some(bind) = &cfg.bind_address {
            let local = resolve_bind_addr(bind, addr).await?;
            let socket = if addr.is_ipv4() {
                TcpSocket::new_v4()
            } else {
                TcpSocket::new_v6()
            }
            .map_err(|e| Error::Network(format!("create socket: {e}")))?;
            socket
                .bind(local)
                .map_err(|e| Error::Network(format!("bind to {local}: {e}")))?;
            socket
                .connect(addr)
                .await
                .map_err(|e| Error::Network(format!("connect to {addr}: {e}")))
        } else {
            TcpStream::connect(addr)
                .await
                .map_err(|e| Error::Network(format!("connect to {addr}: {e}")))
        }
    };

    let stream = if let Some(t) = connect_timeout {
        timeout(t, fut)
            .await
            .map_err(|_| Error::Network(format!("connect timeout to {addr}")))?
    } else {
        fut.await
    }?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn resolve_bind_addr(bind: &str, peer: SocketAddr) -> Result<SocketAddr> {
    if let Ok(ip) = bind.parse::<IpAddr>() {
        if ip.is_ipv4() != peer.is_ipv4() {
            return Err(Error::Network(format!(
                "bind-address {bind} family does not match peer {peer}"
            )));
        }
        return Ok(SocketAddr::new(ip, 0));
    }
    let candidates: Vec<SocketAddr> = tokio::net::lookup_host((bind, 0u16))
        .await
        .map_err(|e| Error::Network(format!("bind-address DNS lookup failed for {bind}: {e}")))?
        .collect();
    candidates
        .into_iter()
        .find(|a| a.is_ipv4() == peer.is_ipv4())
        .ok_or_else(|| {
            Error::Network(format!(
                "bind-address {bind} has no address matching peer family {peer}"
            ))
        })
}

/// Connect using a simplified Happy Eyeballs race (RFC 8305).
///
/// Tries the preferred address family immediately and the other family after
/// 250ms; first successful connection wins. Falls back to sequential tries when
/// only one family is present. When `retry_connrefused` is false, a connection
/// refused error aborts instead of trying the next address.
///
/// # Errors
///
/// Returns [`Error::Network`] when no address
/// connects.
pub async fn connect_happy_eyeballs(cfg: &Config, addrs: &[SocketAddr]) -> Result<TcpStream> {
    if addrs.is_empty() {
        return Err(Error::Network("no addresses to connect".into()));
    }

    let v4: Vec<SocketAddr> = addrs.iter().copied().filter(|a| a.is_ipv4()).collect();
    let v6: Vec<SocketAddr> = addrs.iter().copied().filter(|a| a.is_ipv6()).collect();

    if v4.is_empty() || v6.is_empty() {
        let mut last_err = None;
        for addr in addrs {
            match connect_tcp(cfg, *addr).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    if !cfg.retry_connrefused && e.to_string().contains("Connection refused") {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        return Err(last_err.unwrap_or_else(|| Error::Network("connect failed".into())));
    }

    let prefer_v4 = cfg.prefer_family.eq_ignore_ascii_case("IPv4");
    let (first, second) = if prefer_v4 {
        (v4[0], v6[0])
    } else {
        (v6[0], v4[0])
    };

    let cfg1 = cfg;
    let attempt_first = connect_tcp(cfg1, first);
    let attempt_second = async {
        tokio::time::sleep(Duration::from_millis(250)).await;
        connect_tcp(cfg1, second).await
    };

    tokio::select! {
        res = attempt_first => {
            match res {
                Ok(s) => Ok(s),
                Err(e1) => {
                    if let Ok(s) = connect_tcp(cfg, second).await {
                        return Ok(s);
                    }
                    let rest: Vec<_> = addrs
                        .iter()
                        .copied()
                        .filter(|a| *a != first && *a != second)
                        .collect();
                    if rest.is_empty() {
                        return Err(e1);
                    }
                    Box::pin(connect_happy_eyeballs(cfg, &rest)).await
                }
            }
        }
        res = attempt_second => res,
    }
}

/// Token-bucket download rate limiter.
///
/// Tokens refill from elapsed time and are capped at `2 * bytes_per_sec`.
/// [`RateLimiter::new`] returns `None` when the rate is unset. A rate of `0` is
/// treated as 1 byte/s.
pub struct RateLimiter {
    bytes_per_sec: u64,
    allowed: u64,
    last: std::time::Instant,
}

impl RateLimiter {
    /// Create a limiter for `bytes_per_sec`, or `None` if the rate is unset.
    pub fn new(bytes_per_sec: Option<u64>) -> Option<Self> {
        bytes_per_sec.map(|b| Self {
            bytes_per_sec: b.max(1),
            allowed: b.max(1),
            last: std::time::Instant::now(),
        })
    }

    /// Consume `n` bytes of quota, sleeping until tokens are available.
    pub async fn take(&mut self, n: u64) {
        self.refill();
        while self.allowed < n {
            let need = n - self.allowed;
            let sleep_ms = ((need as u128) * 1000) / self.bytes_per_sec as u128;
            tokio::time::sleep(Duration::from_millis(sleep_ms.max(1) as u64)).await;
            self.refill();
        }
        self.allowed -= n;
    }

    fn refill(&mut self) {
        let elapsed = self.last.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            let add = (elapsed * self.bytes_per_sec as f64) as u64;
            self.allowed = (self.allowed + add).min(self.bytes_per_sec * 2);
            self.last = std::time::Instant::now();
        }
    }
}

/// Proxy URL for `scheme` (`http`, `https`, or `ftp`).
///
/// Returns `None` unless [`fetchling_core::Config::use_proxy`] is true. Prefers
/// `http_proxy` / `https_proxy` on [`fetchling_core::Config`], then the matching
/// environment variable in either case (`http_proxy` / `HTTP_PROXY`).
pub fn proxy_url_for(cfg: &Config, scheme: &str) -> Option<String> {
    if !cfg.use_proxy {
        return None;
    }
    let from_cfg = match scheme {
        "https" => cfg.https_proxy.as_ref(),
        "http" => cfg.http_proxy.as_ref(),
        _ => None,
    };
    if let Some(v) = from_cfg.filter(|s| !s.is_empty()) {
        return Some(v.clone());
    }
    let key = match scheme {
        "https" => "https_proxy",
        "http" => "http_proxy",
        "ftp" => "ftp_proxy",
        _ => return None,
    };
    std::env::var(key)
        .or_else(|_| std::env::var(key.to_ascii_uppercase()))
        .ok()
        .filter(|s| !s.is_empty())
}

/// Normalized `scheme://host:port` key for connection pooling.
///
/// Empty or missing URLs yield an empty string. Unparseable values are returned
/// unchanged.
pub fn proxy_endpoint_key(proxy_url: Option<&str>) -> String {
    let Some(raw) = proxy_url.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    match Url::parse(raw) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("");
            let port = u.port_or_known_default().unwrap_or(0);
            format!("{}://{host}:{port}", u.scheme())
        }
        Err(_) => raw.to_string(),
    }
}

/// TCP-connect to an HTTP proxy and return the stream plus parsed proxy URL.
///
/// Does not send CONNECT; use this for plain HTTP absolute-form requests.
/// For HTTPS through a proxy, use [`connect_via_http_connect`].
///
/// # Errors
///
/// Returns [`Error::Network`] for a bad proxy
/// URL, DNS failure, or TCP connect failure.
pub async fn connect_to_proxy(
    cfg: &Config,
    dns: &DnsCache,
    proxy_url_str: &str,
) -> Result<(TcpStream, Url)> {
    let proxy = Url::parse(proxy_url_str)
        .map_err(|e| Error::Network(format!("bad proxy URL '{proxy_url_str}': {e}")))?;
    let phost = proxy
        .host_str()
        .ok_or_else(|| Error::Network(format!("proxy URL missing host: {proxy_url_str}")))?;
    let pport = proxy
        .port_or_known_default()
        .ok_or_else(|| Error::Network(format!("proxy URL missing port: {proxy_url_str}")))?;
    let addrs = dns.lookup(cfg, phost, pport).await?;
    let stream = connect_happy_eyeballs(cfg, &addrs).await?;
    Ok((stream, proxy))
}

/// Absolute-form request target for HTTP requests sent through a proxy.
///
/// IPv6 hosts are wrapped in brackets when the URL host is not already
/// bracketed.
///
/// # Examples
///
/// ```
/// use fetchling_net::absolute_http_request_target;
/// use url::Url;
///
/// let url = Url::parse("http://example.com:8080/a/b?x=1").unwrap();
/// assert_eq!(
///     absolute_http_request_target(&url),
///     "http://example.com:8080/a/b?x=1"
/// );
/// ```
pub fn absolute_http_request_target(url: &Url) -> String {
    let mut s = format!("{}://", url.scheme());
    if let Some(host) = url.host_str() {
        if host.contains(':') && !host.starts_with('[') {
            s.push('[');
            s.push_str(host);
            s.push(']');
        } else {
            s.push_str(host);
        }
    }
    if let Some(port) = url.port() {
        s.push(':');
        s.push_str(&port.to_string());
    }
    let path = url.path();
    if path.is_empty() {
        s.push('/');
    } else {
        s.push_str(path);
    }
    if let Some(q) = url.query() {
        s.push('?');
        s.push_str(q);
    }
    s
}

/// Whether `host` is listed in the `no_proxy` / `NO_PROXY` environment variable.
pub fn proxy_bypassed(host: &str) -> bool {
    let list = match std::env::var("no_proxy").or_else(|_| std::env::var("NO_PROXY")) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return false,
    };
    host_matches_no_proxy(host, &list)
}

/// Whether `host` matches a `no_proxy`-style comma/space-separated `list`.
///
/// `*` matches every host. Entries match an exact hostname or a suffix
/// (`example.com` matches `a.example.com`). A leading `.` on an entry is
/// ignored.
///
/// # Examples
///
/// ```
/// use fetchling_net::host_matches_no_proxy;
///
/// assert!(host_matches_no_proxy("example.com", "example.com"));
/// assert!(host_matches_no_proxy("a.example.com", ".example.com"));
/// assert!(!host_matches_no_proxy("evil-example.com", "example.com"));
/// assert!(host_matches_no_proxy("anything", "*"));
/// ```
pub fn host_matches_no_proxy(host: &str, list: &str) -> bool {
    let host = host.trim().trim_matches(|c| c == '[' || c == ']');
    let host_l = host.to_ascii_lowercase();
    for entry in list.split([',', ' ']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        let entry = entry.strip_prefix('.').unwrap_or(entry);
        let entry_l = entry.to_ascii_lowercase();
        if host_l == entry_l || host_l.ends_with(&format!(".{entry_l}")) {
            return true;
        }
    }
    false
}

/// Format an HTTP `CONNECT` request, optionally with Basic proxy authorization.
///
/// IPv6 hosts are written as `[host]:port`.
///
/// # Examples
///
/// ```
/// use fetchling_net::format_http_connect_request;
///
/// let req = format_http_connect_request("example.com", 443, None);
/// assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
/// assert!(req.contains("Host: example.com:443\r\n"));
/// assert!(req.ends_with("\r\n\r\n"));
/// ```
pub fn format_http_connect_request(
    target_host: &str,
    target_port: u16,
    proxy_authorization: Option<&str>,
) -> String {
    let authority = connect_authority(target_host, target_port);
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = proxy_authorization {
        req.push_str("Proxy-Authorization: Basic ");
        req.push_str(auth);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req
}

fn connect_authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Base64 `user:pass` for `Proxy-Authorization: Basic`.
///
/// Prefers [`fetchling_core::Config::proxy_user`] /
/// [`fetchling_core::Config::proxy_password`], then userinfo on `proxy_url`.
/// Returns `None` when no username is available. A missing password is treated
/// as empty.
pub fn proxy_basic_auth(cfg: &Config, proxy_url: &Url) -> Option<String> {
    let user = cfg.proxy_user.clone().or_else(|| {
        let u = proxy_url.username();
        if u.is_empty() {
            None
        } else {
            Some(u.to_string())
        }
    })?;
    let pass = cfg
        .proxy_password
        .clone()
        .or_else(|| proxy_url.password().map(|p| p.to_string()))
        .unwrap_or_default();
    use base64::Engine;
    Some(base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}").as_bytes()))
}

/// Open a TCP tunnel through an HTTP proxy with `CONNECT`.
///
/// Succeeds only on a 2xx CONNECT response. Use this for HTTPS (or other
/// tunneled) traffic; for plain HTTP via proxy, use [`connect_to_proxy`] and
/// [`absolute_http_request_target`].
///
/// # Errors
///
/// Returns [`Error::Network`] for a bad proxy
/// URL, DNS/TCP failure, a truncated CONNECT exchange, or a non-2xx status.
pub async fn connect_via_http_connect(
    cfg: &Config,
    dns: &DnsCache,
    proxy_url_str: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let proxy = Url::parse(proxy_url_str)
        .map_err(|e| Error::Network(format!("bad proxy URL '{proxy_url_str}': {e}")))?;
    let phost = proxy
        .host_str()
        .ok_or_else(|| Error::Network(format!("proxy URL missing host: {proxy_url_str}")))?;
    let pport = proxy
        .port_or_known_default()
        .ok_or_else(|| Error::Network(format!("proxy URL missing port: {proxy_url_str}")))?;
    let addrs = dns.lookup(cfg, phost, pport).await?;
    let mut stream = connect_happy_eyeballs(cfg, &addrs).await?;
    let auth = proxy_basic_auth(cfg, &proxy);
    let req = format_http_connect_request(target_host, target_port, auth.as_deref());
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| Error::Network(format!("proxy CONNECT write: {e}")))?;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| Error::Network(format!("proxy CONNECT read: {e}")))?;
        if n == 0 {
            return Err(Error::Network(
                "proxy closed connection during CONNECT".into(),
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(Error::Network("proxy CONNECT response too large".into()));
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());
    match code {
        Some(c) if (200..300).contains(&c) => Ok(stream),
        _ => Err(Error::Network(format!(
            "proxy CONNECT failed: {status_line}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    #[test]
    fn parse_nameserver_ip_and_port() {
        assert_eq!(
            parse_nameserver("8.8.8.8").unwrap(),
            "8.8.8.8:53".parse().unwrap()
        );
        assert_eq!(
            parse_nameserver("1.1.1.1:5353").unwrap(),
            "1.1.1.1:5353".parse().unwrap()
        );
        assert_eq!(
            parse_nameserver("[::1]:53").unwrap(),
            "[::1]:53".parse().unwrap()
        );
        assert!(matches!(parse_nameserver("hostname"), Err(Error::Parse(_))));
    }

    #[test]
    fn custom_resolver_rejects_bad_config() {
        let cfg = Config {
            dns_servers: Some("  ,  ".into()),
            ..Config::default()
        };
        assert!(matches!(build_custom_resolver(&cfg), Err(Error::Parse(_))));
        let cfg = Config {
            bind_dns_address: Some("not-an-ip".into()),
            ..Config::default()
        };
        assert!(matches!(build_custom_resolver(&cfg), Err(Error::Parse(_))));
    }

    #[test]
    fn read_timeout_dur_from_config() {
        assert_eq!(
            read_timeout_dur(&Config::default()),
            Some(Duration::from_secs_f64(900.0))
        );
        let cfg = Config {
            read_timeout: Some(10.0),
            ..Config::default()
        };
        assert_eq!(read_timeout_dur(&cfg), Some(Duration::from_secs_f64(10.0)));
        let cfg = Config {
            read_timeout: None,
            timeout: None,
            ..Config::default()
        };
        assert!(read_timeout_dur(&cfg).is_none());
        let cfg = Config {
            read_timeout: Some(0.0),
            timeout: Some(5.0),
            ..Config::default()
        };
        assert!(read_timeout_dur(&cfg).is_none());
        let cfg = Config {
            read_timeout: None,
            timeout: Some(5.0),
            ..Config::default()
        };
        assert_eq!(read_timeout_dur(&cfg), Some(Duration::from_secs_f64(5.0)));
    }

    #[test]
    fn rate_limiter_new_from_option() {
        assert!(RateLimiter::new(None).is_none());
        assert!(RateLimiter::new(Some(0)).is_some());
        assert!(RateLimiter::new(Some(1024)).is_some());
    }

    #[test]
    fn no_proxy_matching() {
        assert!(host_matches_no_proxy("example.com", "example.com"));
        assert!(host_matches_no_proxy("a.example.com", ".example.com"));
        assert!(host_matches_no_proxy("a.example.com", "example.com"));
        assert!(!host_matches_no_proxy("evil-example.com", "example.com"));
        assert!(host_matches_no_proxy("anything", "*"));
        assert!(host_matches_no_proxy("[2001:db8::1]", "2001:db8::1"));
        assert!(host_matches_no_proxy(
            "a.example.com",
            "other.com, EXAMPLE.COM"
        ));
        assert!(!host_matches_no_proxy("example.com", " , "));
        assert!(host_matches_no_proxy("WWW.EXAMPLE.COM", "example.com"));
    }

    #[test]
    fn connect_request_format_and_auth() {
        let req = format_http_connect_request("example.com", 443, None);
        assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com:443\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
        let req = format_http_connect_request("example.com", 443, Some("dXNlcjpwYXNz"));
        assert!(req.contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n"));
        let req = format_http_connect_request("2001:db8::1", 443, None);
        assert!(req.contains("CONNECT [2001:db8::1]:443 "));
    }

    #[test]
    fn proxy_url_prefers_config_over_env() {
        let mut cfg = Config {
            http_proxy: Some("http://cfg-proxy:8080".into()),
            https_proxy: Some("http://https-proxy:8443".into()),
            ..Config::default()
        };
        assert_eq!(
            proxy_url_for(&cfg, "http").as_deref(),
            Some("http://cfg-proxy:8080")
        );
        assert_eq!(
            proxy_url_for(&cfg, "https").as_deref(),
            Some("http://https-proxy:8443")
        );
        assert!(proxy_url_for(&cfg, "gopher").is_none());
        cfg.http_proxy = Some(String::new());
        assert_ne!(proxy_url_for(&cfg, "http").as_deref(), Some(""));
        cfg.use_proxy = false;
        assert!(proxy_url_for(&cfg, "http").is_none());
        assert!(proxy_url_for(&cfg, "ftp").is_none());
    }

    #[test]
    fn proxy_basic_auth_sources() {
        let url = Url::parse("http://alice:secret@proxy.example:8080").unwrap();
        let cfg = Config::default();
        assert_eq!(
            proxy_basic_auth(&cfg, &url).as_deref(),
            Some("YWxpY2U6c2VjcmV0")
        );
        let cfg = Config {
            proxy_user: Some("bob".into()),
            proxy_password: Some("pw".into()),
            ..Config::default()
        };
        assert_eq!(proxy_basic_auth(&cfg, &url).as_deref(), Some("Ym9iOnB3"));
        let url = Url::parse("http://proxy.example:8080").unwrap();
        assert!(proxy_basic_auth(&Config::default(), &url).is_none());
        let url = Url::parse("http://alice@proxy.example:8080").unwrap();
        assert_eq!(
            proxy_basic_auth(&Config::default(), &url).as_deref(),
            Some("YWxpY2U6")
        );
    }

    #[test]
    fn absolute_http_request_target_format() {
        let u = Url::parse("http://example.com:8080/a/b?x=1").unwrap();
        assert_eq!(
            absolute_http_request_target(&u),
            "http://example.com:8080/a/b?x=1"
        );
        let u = Url::parse("http://example.com/").unwrap();
        assert_eq!(absolute_http_request_target(&u), "http://example.com/");
        let u = Url::parse("http://[2001:db8::1]/x").unwrap();
        assert_eq!(absolute_http_request_target(&u), "http://[2001:db8::1]/x");
        let u = Url::parse("http://example.com").unwrap();
        assert_eq!(absolute_http_request_target(&u), "http://example.com/");
    }

    #[test]
    fn proxy_endpoint_key_normalizes() {
        assert_eq!(proxy_endpoint_key(None), "");
        assert_eq!(proxy_endpoint_key(Some("")), "");
        assert_eq!(
            proxy_endpoint_key(Some("http://proxy.example:8080/")),
            "http://proxy.example:8080"
        );
        assert_eq!(
            proxy_endpoint_key(Some("http://proxy.example")),
            "http://proxy.example:80"
        );
        assert_eq!(proxy_endpoint_key(Some("not a url")), "not a url");
    }

    #[tokio::test]
    async fn dns_cache_lookup_literals_and_filters() {
        let dns = DnsCache::new();
        let cfg = Config::default();
        let addrs = dns.lookup(&cfg, "127.0.0.1", 9).await.unwrap();
        assert_eq!(
            addrs,
            vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9)]
        );
        {
            let guard = dns.inner.lock().unwrap();
            assert!(guard.contains_key("127.0.0.1:9"));
        }
        let again = dns.lookup(&cfg, "127.0.0.1", 9).await.unwrap();
        assert_eq!(addrs, again);

        let cfg = Config {
            inet6_only: true,
            dns_cache: false,
            ..Config::default()
        };
        let err = dns.lookup(&cfg, "127.0.0.1", 9).await.unwrap_err();
        assert!(matches!(err, Error::Network(_)));

        let cfg = Config {
            dns_servers: Some("127.0.0.1".into()),
            dns_cache: false,
            ..Config::default()
        };
        let addrs = DnsCache::new().lookup(&cfg, "192.0.2.1", 80).await.unwrap();
        assert_eq!(addrs, vec!["192.0.2.1:80".parse().unwrap()]);
    }

    #[tokio::test]
    async fn rate_limiter_take_with_high_rate() {
        let mut limiter = RateLimiter::new(Some(1_000_000)).unwrap();
        limiter.take(16).await;
    }

    #[tokio::test]
    async fn connect_tcp_and_happy_eyeballs_localhost() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let stream = connect_tcp(&Config::default(), addr).await.unwrap();
        drop(stream);
        accept.await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let stream = connect_happy_eyeballs(&Config::default(), &[addr])
            .await
            .unwrap();
        drop(stream);
        accept.await.unwrap();

        let err = connect_happy_eyeballs(&Config::default(), &[])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[tokio::test]
    async fn connect_to_proxy_localhost() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let proxy = format!("http://127.0.0.1:{}", addr.port());
        let (_stream, url) = connect_to_proxy(&Config::default(), &DnsCache::new(), &proxy)
            .await
            .unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(addr.port()));
        accept.await.unwrap();
    }

    async fn serve_connect(listener: TcpListener, response: &'static [u8]) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 512];
        loop {
            let n = stream.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(response).await.unwrap();
    }

    #[tokio::test]
    async fn connect_via_http_connect_status_and_url() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(serve_connect(
            listener,
            b"HTTP/1.1 200 Connection Established\r\n\r\n",
        ));
        let proxy = format!("http://127.0.0.1:{}", addr.port());
        connect_via_http_connect(
            &Config::default(),
            &DnsCache::new(),
            &proxy,
            "example.com",
            443,
        )
        .await
        .unwrap();
        accept.await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(serve_connect(
            listener,
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
        ));
        let proxy = format!("http://127.0.0.1:{}", addr.port());
        let err = connect_via_http_connect(
            &Config::default(),
            &DnsCache::new(),
            &proxy,
            "example.com",
            443,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
        accept.await.unwrap();

        let err = connect_via_http_connect(
            &Config::default(),
            &DnsCache::new(),
            "not-a-url",
            "example.com",
            443,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }
}
