//! Recursive HTTP/FTP retrieval orchestration with robots, metalink, and path policy.
//!
//! # What this crate is (and is not)
//!
//! IS: an async job runner ([`Engine`]) that takes a filled [`Config`], queues
//! URLs, retrieves over HTTP/HTTPS and FTP/FTPS (via `fetchling-http` /
//! `fetchling-ftp`), applies concurrency limits, follows robots/sitemaps,
//! extracts links for recursion, handles Metalink and WARC, and saves
//! cookies/HSTS. Destination helpers ([`local_path_for_url`],
//! [`resolve_dest_path`], [`DestAction`], [`unique_path`],
//! [`should_skip_clobber`], [`rotate_backups`], [`finalize_download_path`],
//! [`ensure_parent`]). The public API is flat at the crate root. Drive behavior
//! with [`Config`] fields set directly; CLI / wget names in those field docs
//! are compatibility aliases. [`Engine::run`] needs a Tokio runtime.
//!
//! IS NOT: CLI/argv parsing (`fetchling-cli`), an HTTP or FTP client
//! implementation, or HTML/CSS/Metalink/WARC parsers (`fetchling-formats`).
//! This crate does not re-export [`Config`], [`Error`], [`Logger`],
//! [`ExitCode`], `HttpClient`, or `FtpClient`.
//!
//! # Typical integration
//!
//! 1. Start from [`Config::default`] and set `urls` plus retrieval fields
//!    (`recursive`, `directory_prefix`, proxies, TLS, `continue_download`, …)
//! 2. Create [`Engine::new`]
//! 3. Call [`Engine::run`]
//! 4. Optionally use destination helpers yourself without constructing [`Engine`]
//!
//! # Areas
//!
//! - **Engine** — [`Engine`]
//! - **Destination** — [`DestAction`], [`local_path_for_url`], [`ensure_parent`],
//!   [`unique_path`], [`resolve_dest_path`], [`should_skip_clobber`],
//!   [`rotate_backups`], [`finalize_download_path`]
//!
//! # Examples
//!
//! Map a URL to a local path (no network):
//!
//! ```
//! use std::path::PathBuf;
//! use fetchling_core::Config;
//! use fetchling_engine::local_path_for_url;
//! use url::Url;
//!
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! let url = Url::parse("http://example.com/a/b.txt").unwrap();
//! assert_eq!(local_path_for_url(&cfg, &url), PathBuf::from("./b.txt"));
//! ```
//!
//! Construct an engine from default config (no network):
//!
//! ```
//! use fetchling_core::Config;
//! use fetchling_engine::Engine;
//!
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! let engine = Engine::new(cfg).unwrap();
//! let _ = engine;
//! ```
//!
//! Run a retrieval (does not run; needs a server):
//!
//! ```no_run
//! use fetchling_core::Config;
//! use fetchling_engine::Engine;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! cfg.urls = vec!["https://example.com/file.bin".into()];
//! let code = Engine::new(cfg).unwrap().run().await.unwrap();
//! let _ = code;
//! # }
//! ```

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fetchling_core::{
    charset_from_content_type, decode_bytes, normalize_url_iri, Config, Error, ExitCode, Logger,
    Result,
};
use fetchling_formats::{
    convert_links, decode_hashes, encode_hashes, extract_atom_urls, extract_css_urls,
    extract_html_urls, extract_rss_urls, extract_sitemap_urls, is_metalink_mediatype,
    parse_link_headers, parse_metalink_doc, HtmlExtractOpts, MetalinkHash, Robots, WarcWriter,
};
use fetchling_ftp::{FtpClient, FtpDownloadOutcome, FtpEntry, FtpEntryKind};
use fetchling_http::HttpClient;
use md5::Md5;
use regex::Regex;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use url::Url;

mod askpass;
mod fs;

pub use fs::{
    ensure_parent, finalize_download_path, local_path_for_url, resolve_dest_path, rotate_backups,
    should_skip_clobber, unique_path, DestAction,
};

/// Async retrieval job runner.
///
/// Owns a [`Config`] and [`Logger`]. HTTP/FTP clients, WARC, the URL queue, and
/// worker tasks are constructed inside [`Self::run`].
pub struct Engine {
    cfg: Config,
    log: Logger,
}

/// Robots.txt cache entry for singleflight fetches.
enum RobotsEntry {
    Pending(Arc<Notify>),
    Ready(Robots),
}

struct Shared {
    cfg: Config,
    log: Logger,
    http: HttpClient,
    ftp: FtpClient,
    warc: Mutex<Option<WarcWriter>>,
    queue: Mutex<VecDeque<(Url, i32)>>,
    visited: Mutex<HashSet<String>>,
    link_map: Mutex<Vec<(String, String)>>,
    robots_cache: Mutex<HashMap<String, RobotsEntry>>,
    host_semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    path_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    metalink_hashes: Mutex<HashMap<String, String>>,
    metalink_mirrors: Mutex<HashMap<String, Vec<Url>>>,
    sitemaps_seeded: Mutex<HashSet<String>>,
    downloaded: AtomicU64,
    quota_reached: AtomicBool,
    worst: Mutex<ExitCode>,
}

impl Engine {
    /// Build an engine from `cfg`.
    ///
    /// Creates a [`Logger`] from the same config.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the logfile cannot be opened.
    pub fn new(cfg: Config) -> Result<Self> {
        let log = Logger::new(&cfg)?;
        Ok(Self { cfg, log })
    }

    /// Run the retrieval to completion, consuming `self`.
    ///
    /// Seeds the queue from `cfg.urls`, input files, and metalink; retrieves
    /// concurrently; follows robots and recursion; optionally converts links
    /// and saves cookies/HSTS.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Parse`] when no URL is specified or `--base` is
    /// invalid; [`Error::Auth`] on askpass failure; [`Error::Io`] on local
    /// filesystem errors; plus retrieve failures ([`Error::Protocol`],
    /// [`Error::Server`], [`Error::Network`], [`Error::Tls`]).
    pub async fn run(self) -> Result<ExitCode> {
        let mut cfg = self.cfg;
        askpass::maybe_prompt_password(&mut cfg)?;
        if cfg.hsts && cfg.hsts_file.is_none() {
            if let Some(home) = std::env::var_os("HOME") {
                cfg.hsts_file = Some(
                    std::path::PathBuf::from(home)
                        .join(".fetchling-hsts")
                        .display()
                        .to_string(),
                );
            }
        }
        warn_unimplemented_stubs(&cfg);
        if let Some(enc) = &cfg.local_encoding {
            fetchling_core::resolve_encoding(enc)?;
        }
        if let Some(enc) = &cfg.remote_encoding {
            fetchling_core::resolve_encoding(enc)?;
        }

        let mut urls = cfg.urls.clone();
        let mut metalink_hashes: HashMap<String, String> = HashMap::new();
        let mut metalink_mirrors: HashMap<String, Vec<Url>> = HashMap::new();
        let base_url = cfg
            .base
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|e| Error::Parse(format!("bad --base URL: {e}")))?;
        if let Some(input) = cfg.input_file.clone() {
            let text = if input == "-" {
                use std::io::Read;
                let mut buf = Vec::new();
                std::io::stdin().read_to_end(&mut buf)?;
                decode_bytes(&buf, cfg.local_encoding.as_deref())?
            } else {
                let buf = std::fs::read(&input)?;
                decode_bytes(&buf, cfg.local_encoding.as_deref())?
            };
            if cfg.force_metalink {
                ingest_metalink_xml(
                    &text,
                    &cfg,
                    &mut urls,
                    &mut metalink_hashes,
                    &mut metalink_mirrors,
                )?;
            } else if cfg.force_html
                || cfg.force_css
                || cfg.force_rss
                || cfg.force_atom
                || cfg.force_sitemap
            {
                let html_base = base_url
                    .clone()
                    .unwrap_or_else(|| Url::parse("file:///").expect("static URL"));
                if cfg.force_html {
                    for u in extract_html_urls(
                        &html_base,
                        &text,
                        HtmlExtractOpts {
                            follow_tags: &cfg.follow_tags,
                            ignore_tags: &cfg.ignore_tags,
                            strict_comments: cfg.strict_comments,
                        },
                    ) {
                        urls.push(u.as_str().to_string());
                    }
                }
                if cfg.force_css {
                    for u in extract_css_urls(&html_base, &text) {
                        urls.push(u.as_str().to_string());
                    }
                }
                if cfg.force_rss {
                    for u in extract_rss_urls(&html_base, &text) {
                        urls.push(u.as_str().to_string());
                    }
                }
                if cfg.force_atom {
                    for u in extract_atom_urls(&html_base, &text) {
                        urls.push(u.as_str().to_string());
                    }
                }
                if cfg.force_sitemap {
                    for u in extract_sitemap_urls(&html_base, &text) {
                        urls.push(u.as_str().to_string());
                    }
                }
            } else {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    urls.push(resolve_input_url(line, base_url.as_ref())?);
                }
            }
        }

        if let Some(meta) = cfg.input_metalink.clone() {
            let buf = std::fs::read(&meta)?;
            let xml = decode_bytes(&buf, cfg.local_encoding.as_deref())?;
            ingest_metalink_xml(
                &xml,
                &cfg,
                &mut urls,
                &mut metalink_hashes,
                &mut metalink_mirrors,
            )?;
        }

        if urls.is_empty() {
            return Err(Error::Parse("no URL specified".into()));
        }

        let mut initial = VecDeque::new();
        for u in &urls {
            let fu = normalize_url_iri(u, cfg.iri)?;
            initial.push_back((fu.url, 0));
        }

        let http = HttpClient::new(&cfg, self.log.clone())?;
        let warc = WarcWriter::open(&cfg)?;
        let max_threads = cfg.max_threads.max(1) as usize;
        let shared = Arc::new(Shared {
            cfg,
            log: self.log.clone(),
            http,
            ftp: FtpClient::default(),
            warc: Mutex::new(warc),
            queue: Mutex::new(initial),
            visited: Mutex::new(HashSet::new()),
            link_map: Mutex::new(Vec::new()),
            robots_cache: Mutex::new(HashMap::new()),
            host_semaphores: Mutex::new(HashMap::new()),
            path_locks: Mutex::new(HashMap::new()),
            metalink_hashes: Mutex::new(metalink_hashes),
            metalink_mirrors: Mutex::new(metalink_mirrors),
            sitemaps_seeded: Mutex::new(HashSet::new()),
            downloaded: AtomicU64::new(0),
            quota_reached: AtomicBool::new(false),
            worst: Mutex::new(ExitCode::Success),
        });

        let sem = Arc::new(Semaphore::new(max_threads));
        let mut join_set: JoinSet<()> = JoinSet::new();

        loop {
            if shared.quota_reached.load(Ordering::Relaxed) {
                while join_set.join_next().await.is_some() {}
                break;
            }
            loop {
                if shared.quota_reached.load(Ordering::Relaxed) {
                    break;
                }
                let permit = match Arc::clone(&sem).try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let job = {
                    let mut q = shared.queue.lock().await;
                    q.pop_front()
                };
                let Some((url, depth)) = job else {
                    drop(permit);
                    break;
                };

                let state = Arc::clone(&shared);
                join_set.spawn(async move {
                    process_job(state, url, depth, permit).await;
                });
            }

            if join_set.is_empty() {
                break;
            }

            let _ = join_set.join_next().await;
        }

        let link_map = shared.link_map.lock().await.clone();
        if shared.cfg.convert_links {
            for (_url, path) in &link_map {
                let p = PathBuf::from(path);
                match std::fs::read_to_string(&p) {
                    Ok(html) => {
                        if shared.cfg.backup_converted {
                            if let Err(e) = std::fs::copy(&p, format!("{path}.orig")) {
                                shared.log.error(&format!(
                                    "fetchling: convert-links backup {path}.orig: {e}"
                                ));
                            }
                        }
                        let converted =
                            convert_links(&html, &link_map, shared.cfg.convert_file_only);
                        if let Err(e) = std::fs::write(&p, converted) {
                            shared
                                .log
                                .error(&format!("fetchling: convert-links write {path}: {e}"));
                        }
                    }
                    Err(e) => {
                        shared
                            .log
                            .error(&format!("fetchling: convert-links read {path}: {e}"));
                    }
                }
            }
        }

        if shared.cfg.cookies {
            if let Some(path) = &shared.cfg.save_cookies {
                shared
                    .http
                    .save_cookies(Path::new(path), shared.cfg.keep_session_cookies)?;
            }
        }
        if shared.cfg.hsts {
            if let Some(path) = &shared.cfg.hsts_file {
                shared.http.save_hsts(path)?;
            }
        }

        if shared.cfg.warc_keep_log {
            if let Some(log_path) = &shared.cfg.logfile {
                let mut warc = shared.warc.lock().await;
                if let Some(w) = warc.as_mut() {
                    match std::fs::read(log_path) {
                        Ok(bytes) if !bytes.is_empty() => {
                            if let Err(e) = w.write_resource("metadata://fetchling/log", &bytes) {
                                shared
                                    .log
                                    .error(&format!("fetchling: WARC keep-log failed: {e}"));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            shared
                                .log
                                .error(&format!("fetchling: WARC keep-log read {log_path}: {e}"));
                        }
                    }
                }
            }
        }

        let worst = *shared.worst.lock().await;
        Ok(worst)
    }
}

async fn host_semaphore(state: &Shared, host: &str) -> Arc<Semaphore> {
    let per_host = state.cfg.effective_max_threads_per_host().max(1) as usize;
    let mut map = state.host_semaphores.lock().await;
    Arc::clone(
        map.entry(host.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(per_host))),
    )
}

async fn path_lock(state: &Shared, dest: &Path) -> Arc<Mutex<()>> {
    let mut map = state.path_locks.lock().await;
    Arc::clone(
        map.entry(dest.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// Ensure robots.txt for the URL's origin is fetched at most once (singleflight).
///
/// Preserves scheme/host/port from `url` so non-default ports (e.g. test servers)
/// fetch `{origin}/robots.txt` rather than falling back to port 80/443.
async fn ensure_robots(state: &Shared, url: &Url) -> Robots {
    let Some(host) = url.host_str() else {
        return Robots::default();
    };
    let cache_key = url
        .port()
        .map(|p| format!("{}://{host}:{p}", url.scheme()))
        .unwrap_or_else(|| format!("{}://{host}", url.scheme()));

    loop {
        let wait_on = {
            let mut cache = state.robots_cache.lock().await;
            match cache.get(&cache_key) {
                Some(RobotsEntry::Ready(r)) => return r.clone(),
                Some(RobotsEntry::Pending(n)) => Some(Arc::clone(n)),
                None => {
                    let n = Arc::new(Notify::new());
                    cache.insert(cache_key.clone(), RobotsEntry::Pending(Arc::clone(&n)));
                    None
                }
            }
        };

        if let Some(n) = wait_on {
            n.notified().await;
            continue;
        }

        // We are the sole fetcher for this origin.
        let mut robots_url = url.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        let robots = match fetch_robots(state, &robots_url).await {
            Ok(body) => Robots::parse(&body),
            Err(e) => {
                state.log.debug(&format!(
                    "robots.txt fetch failed for {cache_key}: {e}; denying all"
                ));
                Robots::deny_all()
            }
        };

        let notify = {
            let mut cache = state.robots_cache.lock().await;
            let prev = cache.insert(cache_key.clone(), RobotsEntry::Ready(robots.clone()));
            match prev {
                Some(RobotsEntry::Pending(n)) => n,
                _ => Arc::new(Notify::new()),
            }
        };
        notify.notify_waiters();
        return robots;
    }
}

struct RetrieveSuccess {
    url: Url,
    path: PathBuf,
    content_type: Option<String>,
    ftp_entries: Option<Vec<FtpEntry>>,
}

async fn process_job(state: Arc<Shared>, url: Url, depth: i32, _permit: OwnedSemaphorePermit) {
    if quota_exceeded(&state) {
        state.quota_reached.store(true, Ordering::Relaxed);
        return;
    }

    let key = url.as_str().to_string();
    {
        let mut visited = state.visited.lock().await;
        if !visited.insert(key.clone()) {
            return;
        }
    }

    if !accept_url(&state.cfg, &url) {
        log_reject(&state, &url, "accept/reject filters");
        return;
    }

    if state.cfg.recursive || state.cfg.page_requisites {
        if let Some(host) = url.host_str() {
            let robots = ensure_robots(&state, &url).await;
            if !robots.allows(&state.cfg.user_agent, &url) {
                log_reject(&state, &url, "robots.txt");
                return;
            }
            if state.cfg.follow_sitemaps {
                let mut seeded = state.sitemaps_seeded.lock().await;
                // Cache key includes port so http://host:8080 ≠ http://host:80.
                let host_key = url
                    .port()
                    .map(|p| format!("{host}:{p}"))
                    .unwrap_or_else(|| host.to_string());
                if seeded.insert(host_key) {
                    let mut q = state.queue.lock().await;
                    for sm in &robots.sitemaps {
                        if let Ok(u) = Url::parse(sm) {
                            q.push_back((u, depth));
                        }
                    }
                }
            }
        }
    }

    if url.scheme() == "ftp"
        && state.cfg.ftp_glob
        && url.path().bytes().any(|b| matches!(b, b'*' | b'?' | b'['))
    {
        match state.ftp.expand_glob(&state.cfg, &url).await {
            Ok(names) => {
                let mut q = state.queue.lock().await;
                for name in names {
                    if let Some(child) = ftp_join_name(&url, &name) {
                        q.push_back((child, depth));
                    }
                }
            }
            Err(e) => {
                state.log.error(&format!("fetchling: {url}: {e}"));
                let mut worst = state.worst.lock().await;
                *worst = worst.worse(e.exit_code());
            }
        }
        return;
    }

    let host_key = url.host_str().unwrap_or("_").to_string();
    let host_sem = host_semaphore(&state, &host_key).await;
    let Ok(_host_permit) = host_sem.acquire_owned().await else {
        return;
    };

    let mirrors = state
        .metalink_mirrors
        .lock()
        .await
        .remove(url.as_str())
        .unwrap_or_default();
    {
        let mut visited = state.visited.lock().await;
        for m in &mirrors {
            visited.insert(m.as_str().to_string());
        }
    }

    let preferred = local_path_for_url(&state.cfg, &url);
    let mut candidates = Vec::with_capacity(1 + mirrors.len());
    candidates.push(url.clone());
    candidates.extend(mirrors);

    let mut last_err: Option<Error> = None;
    let mut success: Option<RetrieveSuccess> = None;
    for (i, candidate) in candidates.iter().enumerate() {
        match retrieve_one(&state, candidate, &preferred).await {
            Ok((path, content_type, ftp_entries)) => {
                if let Some(expected) = state
                    .metalink_hashes
                    .lock()
                    .await
                    .get(candidate.as_str())
                    .cloned()
                {
                    if let Err(e) = verify_metalink_hashes(&path, &expected, state.cfg.keep_badhash)
                    {
                        if !state.cfg.keep_badhash && i + 1 < candidates.len() {
                            state.log.info(&format!(
                                "metalink hash mismatch for {candidate}; trying next mirror"
                            ));
                            last_err = Some(e);
                            continue;
                        }
                        state.log.error(&format!("fetchling: {candidate}: {e}"));
                        let mut worst = state.worst.lock().await;
                        *worst = worst.worse(e.exit_code());
                        return;
                    }
                }
                success = Some(RetrieveSuccess {
                    url: candidate.clone(),
                    path,
                    content_type,
                    ftp_entries,
                });
                break;
            }
            Err(e) => {
                if i + 1 < candidates.len() {
                    state.log.info(&format!(
                        "metalink mirror {candidate} failed ({e}); trying next"
                    ));
                    last_err = Some(e);
                    continue;
                }
                last_err = Some(e);
            }
        }
    }

    let Some(RetrieveSuccess {
        url: used_url,
        path,
        content_type,
        ftp_entries,
    }) = success
    else {
        if let Some(e) = last_err {
            state.log.error(&format!("fetchling: {url}: {e}"));
            let mut worst = state.worst.lock().await;
            *worst = worst.worse(e.exit_code());
        }
        return;
    };

    if !mime_type_allowed(&state.cfg.filter_mime_type, content_type.as_deref()) {
        let _ = std::fs::remove_file(&path);
        log_reject(&state, &used_url, "filter-mime-type");
        return;
    }

    {
        state
            .link_map
            .lock()
            .await
            .push((used_url.as_str().to_string(), path.display().to_string()));

        let max = if state.cfg.level < 0 {
            i32::MAX
        } else {
            state.cfg.level
        };
        let should_recurse =
            (state.cfg.recursive && depth < max) || (state.cfg.page_requisites && depth < 1);
        if should_recurse {
            if let Some(entries) = ftp_entries {
                enqueue_ftp_listing_entries(&state, &used_url, &preferred, &entries, depth).await;
            } else if !is_metalink_content_type(content_type.as_deref()) {
                if let Ok(raw) = std::fs::read(&path) {
                    let enc_label =
                        state.cfg.remote_encoding.clone().or_else(|| {
                            content_type.as_deref().and_then(charset_from_content_type)
                        });
                    if let Ok(body) = decode_bytes(&raw, enc_label.as_deref()) {
                        let ct_html = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "html" | "htm"))
                            .unwrap_or(false)
                            || body.contains("<html")
                            || body.contains("<HTML");
                        let ct_css = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("css"))
                            .unwrap_or(false);
                        let mut found = Vec::new();
                        if ct_html || state.cfg.force_html {
                            found.extend(extract_html_urls(
                                &used_url,
                                &body,
                                HtmlExtractOpts {
                                    follow_tags: &state.cfg.follow_tags,
                                    ignore_tags: &state.cfg.ignore_tags,
                                    strict_comments: state.cfg.strict_comments,
                                },
                            ));
                        }
                        if ct_css || state.cfg.force_css {
                            found.extend(extract_css_urls(&used_url, &body));
                        }
                        let looks_xml = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("xml"))
                            .unwrap_or(false)
                            || content_type
                                .as_deref()
                                .map(|ct| ct.to_ascii_lowercase().contains("xml"))
                                .unwrap_or(false);
                        if state.cfg.force_rss
                            || (looks_xml
                                && content_type
                                    .as_deref()
                                    .map(|ct| ct.to_ascii_lowercase().contains("rss"))
                                    .unwrap_or(false))
                        {
                            found.extend(extract_rss_urls(&used_url, &body));
                        }
                        if state.cfg.force_atom
                            || (looks_xml
                                && content_type
                                    .as_deref()
                                    .map(|ct| ct.to_ascii_lowercase().contains("atom"))
                                    .unwrap_or(false))
                        {
                            found.extend(extract_atom_urls(&used_url, &body));
                        }
                        if state.cfg.force_sitemap
                            || body.contains("<urlset")
                            || body.contains("<sitemapindex")
                        {
                            found.extend(extract_sitemap_urls(&used_url, &body));
                        }
                        let mut q = state.queue.lock().await;
                        if quota_exceeded(&state) {
                            state.quota_reached.store(true, Ordering::Relaxed);
                        } else {
                            for child in found {
                                let child = match normalize_url_iri(child.as_str(), state.cfg.iri) {
                                    Ok(fu) => fu.url,
                                    Err(_) => {
                                        log_reject(&state, &child, "no-iri");
                                        continue;
                                    }
                                };
                                if state.cfg.https_only && child.scheme() != "https" {
                                    log_reject(&state, &child, "https-only");
                                    continue;
                                }
                                if !state.cfg.span_hosts && used_url.host_str() != child.host_str()
                                {
                                    log_reject(&state, &child, "span-hosts");
                                    continue;
                                }
                                if state.cfg.no_parent && !is_child_path(&used_url, &child) {
                                    log_reject(&state, &child, "no-parent");
                                    continue;
                                }
                                if state.cfg.relative_only
                                    && child.host_str() != used_url.host_str()
                                {
                                    log_reject(&state, &child, "relative-only");
                                    continue;
                                }
                                if !state.cfg.follow_ftp && child.scheme() == "ftp" {
                                    log_reject(&state, &child, "follow-ftp");
                                    continue;
                                }
                                if !accept_url(&state.cfg, &child) {
                                    log_reject(&state, &child, "accept/reject filters");
                                    continue;
                                }
                                q.push_back((child, depth + 1));
                            }
                        }
                    }
                }
            }
        }
        if state.cfg.delete_after {
            let _ = std::fs::remove_file(&path);
        }
    }

    if state.cfg.wait > 0.0 {
        let mut w = state.cfg.wait;
        if state.cfg.random_wait {
            w *= 0.5 + fastrand();
        }
        tokio::time::sleep(Duration::from_secs_f64(w)).await;
    }
}

async fn fetch_robots(state: &Shared, url: &Url) -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("fetchling-robots-{}-{nanos}", std::process::id()));
    let mut cfg = state.cfg.clone();
    cfg.spider = false;
    cfg.recursive = false;
    let result = state.http.download(&cfg, url, &tmp).await;
    let body = match result {
        Ok(_) => std::fs::read_to_string(&tmp).unwrap_or_default(),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&tmp);
    Ok(body)
}

async fn retrieve_one(
    state: &Shared,
    url: &Url,
    preferred: &Path,
) -> Result<(PathBuf, Option<String>, Option<Vec<FtpEntry>>)> {
    let lock = path_lock(state, preferred).await;
    let _guard = lock.lock().await;

    let dest = match resolve_dest_path(&state.cfg, preferred) {
        DestAction::Skip => {
            state.log.info(&format!(
                "File {} already there; not retrieving.",
                preferred.display()
            ));
            return Ok((preferred.to_path_buf(), None, None));
        }
        DestAction::Path(p) => p,
    };

    ensure_parent(&dest)?;
    if dest == preferred
        && preferred.exists()
        && !state.cfg.continue_download
        && !state.cfg.timestamping
    {
        rotate_backups(&state.cfg, &dest)?;
    }

    let tries = if state.cfg.tries == 0 {
        u32::MAX
    } else {
        state.cfg.tries
    };

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let result = match url.scheme() {
            "http" | "https" => {
                let meta = state.http.download(&state.cfg, url, &dest).await;
                match meta {
                    Ok(m) => {
                        {
                            let mut warc = state.warc.lock().await;
                            if let Some(w) = warc.as_mut() {
                                let target = m.final_url.as_str();
                                let peer_ip = m.peer_ip.map(|ip| ip.to_string());
                                let peer_ip = peer_ip.as_deref();
                                let mut req_id = None;
                                if let Some(req) = &m.warc_request {
                                    match w.write_request(target, req, peer_ip) {
                                        Ok(id) => req_id = Some(id),
                                        Err(e) => {
                                            state.log.error(&format!(
                                                "fetchling: WARC write failed: {e}"
                                            ));
                                        }
                                    }
                                }
                                if let Some(resp) = &m.warc_response {
                                    if let Err(e) = w.write_response(
                                        target,
                                        resp,
                                        m.status,
                                        m.content_type.as_deref(),
                                        req_id.as_deref(),
                                        peer_ip,
                                    ) {
                                        state
                                            .log
                                            .error(&format!("fetchling: WARC write failed: {e}"));
                                    }
                                }
                            }
                        }
                        let final_path = if m.status == 304 {
                            dest.clone()
                        } else {
                            finalize_download_path(
                                &state.cfg,
                                url,
                                &dest,
                                &m.final_url,
                                m.content_type.as_deref(),
                                m.content_disposition_filename.as_deref(),
                                m.status,
                            )?
                        };
                        record_downloaded(state, m.bytes_written);
                        if m.status == 304 {
                            state
                                .log
                                .info(&format!("not modified; keeping {}", final_path.display()));
                        } else {
                            state.log.info(&format!(
                                "saved {} bytes to {}",
                                m.bytes_written,
                                final_path.display()
                            ));
                            maybe_set_xattrs(
                                &state.cfg,
                                &final_path,
                                m.final_url.as_str(),
                                Some(url.as_str()),
                                m.content_type.as_deref(),
                            );
                        }
                        if state.cfg.metalink_over_http || state.cfg.force_metalink {
                            let is_metalink_body =
                                is_metalink_content_type(m.content_type.as_deref())
                                    || state.cfg.force_metalink;
                            if is_metalink_body {
                                enqueue_metalink_file(state, &final_path).await?;
                            } else if state.cfg.metalink_over_http && !m.link_headers.is_empty() {
                                apply_metalink_links(state, &m.final_url, &m.link_headers).await?;
                            }
                        }
                        Ok((final_path, m.content_type, None))
                    }
                    Err(e) => Err(e),
                }
            }
            "ftp" | "ftps" => {
                let is_dir = url.path().ends_with('/');
                let ftp_dest = if is_dir {
                    dest.with_file_name(".listing")
                } else {
                    dest.clone()
                };
                state
                    .ftp
                    .download(&state.cfg, url, &ftp_dest)
                    .await
                    .map(|outcome| match outcome {
                        FtpDownloadOutcome::File { bytes } => {
                            record_downloaded(state, bytes);
                            state
                                .log
                                .info(&format!("saved {bytes} bytes to {}", ftp_dest.display()));
                            maybe_set_xattrs(&state.cfg, &ftp_dest, url.as_str(), None, None);
                            (ftp_dest, None, None)
                        }
                        FtpDownloadOutcome::Listing { bytes, entries } => {
                            record_downloaded(state, bytes);
                            if state.cfg.remove_listing {
                                state.log.info(&format!(
                                    "listed {bytes} bytes from {url} (removed .listing)"
                                ));
                            } else {
                                state.log.info(&format!(
                                    "saved {bytes} bytes to {}",
                                    ftp_dest.display()
                                ));
                                maybe_set_xattrs(&state.cfg, &ftp_dest, url.as_str(), None, None);
                            }
                            (ftp_dest, None, Some(entries))
                        }
                    })
            }
            other => Err(Error::Protocol(format!("unsupported scheme: {other}"))),
        };

        match result {
            Ok(p) => return Ok(p),
            Err(e) => {
                if e.is_host_error() && !state.cfg.retry_on_host_error {
                    return Err(e);
                }
                if attempt >= tries {
                    return Err(e);
                }
                let wait = (attempt as f64).min(state.cfg.waitretry);
                state.log.info(&format!(
                    "Retrying {url} ({attempt}/{tries}) after {wait}s: {e}"
                ));
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
            }
        }
    }
}

fn is_metalink_content_type(ct: Option<&str>) -> bool {
    ct.map(is_metalink_mediatype).unwrap_or(false)
}

fn mime_type_allowed(filters: &[String], content_type: Option<&str>) -> bool {
    if filters.is_empty() {
        return true;
    }
    let Some(ct) = content_type.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let media = ct
        .split(';')
        .next()
        .unwrap_or(ct)
        .trim()
        .to_ascii_lowercase();
    filters.iter().any(|f| {
        let f = f.trim().to_ascii_lowercase();
        if f.is_empty() {
            return false;
        }
        if let Some(prefix) = f.strip_suffix("/*") {
            return media.starts_with(&format!("{prefix}/"));
        }
        if !f.contains('/') {
            return media.starts_with(&format!("{f}/"));
        }
        media == f
    })
}

async fn enqueue_metalink_file(state: &Shared, path: &Path) -> Result<()> {
    let xml = std::fs::read_to_string(path)?;
    let preferred = state.cfg.preferred_location.clone();
    let doc = parse_metalink_doc(&xml)?;
    let mut q = state.queue.lock().await;
    let mut hashes = state.metalink_hashes.lock().await;
    let mut mirrors = state.metalink_mirrors.lock().await;
    if state.cfg.metalink_index != 0 {
        if let Some(mu) = doc.select_metaurl(state.cfg.metalink_index) {
            let fu = normalize_url_iri(mu.url.as_str(), state.cfg.iri)?;
            q.push_back((fu.url, 0));
            return Ok(());
        }
    }
    for f in &doc.files {
        let ordered = f.urls_ordered(preferred.as_deref());
        if ordered.is_empty() {
            continue;
        }
        let mut resolved = Vec::new();
        for u in ordered {
            let fu = normalize_url_iri(u.as_str(), state.cfg.iri)?;
            resolved.push(fu.url);
        }
        let primary = resolved[0].clone();
        if !f.hashes.is_empty() {
            let encoded = encode_hashes(&f.hashes);
            for u in &resolved {
                hashes.insert(u.as_str().to_string(), encoded.clone());
            }
        }
        if resolved.len() > 1 {
            mirrors.insert(primary.as_str().to_string(), resolved[1..].to_vec());
        }
        q.push_back((primary, 0));
    }
    Ok(())
}

async fn apply_metalink_links(state: &Shared, base: &Url, headers: &[String]) -> Result<()> {
    let links = parse_link_headers(headers);
    let mut describedby = Vec::new();
    let mut duplicates = Vec::new();
    for link in links {
        match link.rel.as_str() {
            "describedby"
                if link
                    .media_type
                    .as_deref()
                    .map(is_metalink_mediatype)
                    .unwrap_or(false) =>
            {
                describedby.push(link);
            }
            "duplicate" => duplicates.push(link),
            _ => {}
        }
    }
    if !describedby.is_empty() {
        let mut q = state.queue.lock().await;
        for link in describedby {
            let joined = base
                .join(&link.url)
                .map_err(|e| Error::Parse(format!("bad Link URL: {e}")))?;
            q.push_back((joined, 0));
        }
        return Ok(());
    }
    if duplicates.is_empty() {
        return Ok(());
    }
    duplicates.sort_by_key(|l| l.pri.unwrap_or(i32::MAX));
    let mut resolved = Vec::new();
    let mut digests_for_all = Vec::new();
    for link in &duplicates {
        let joined = base
            .join(&link.url)
            .map_err(|e| Error::Parse(format!("bad Link URL: {e}")))?;
        if !link.digests.is_empty() && digests_for_all.is_empty() {
            digests_for_all = link.digests.clone();
        }
        resolved.push(joined);
    }
    let primary = resolved[0].clone();
    {
        let mut hashes = state.metalink_hashes.lock().await;
        if !digests_for_all.is_empty() {
            let encoded = encode_hashes(&digests_for_all);
            for u in &resolved {
                hashes.insert(u.as_str().to_string(), encoded.clone());
            }
        }
    }
    if resolved.len() > 1 {
        state
            .metalink_mirrors
            .lock()
            .await
            .insert(primary.as_str().to_string(), resolved[1..].to_vec());
    }
    state.queue.lock().await.push_back((primary, 0));
    Ok(())
}

fn resolve_input_url(line: &str, base: Option<&Url>) -> Result<String> {
    if line.contains("://") {
        return Ok(line.to_string());
    }
    if let Some(base) = base {
        let joined = base
            .join(line)
            .map_err(|e| Error::Parse(format!("bad input URL relative to --base: {e}")))?;
        return Ok(joined.as_str().to_string());
    }
    Ok(line.to_string())
}

fn quota_exceeded(state: &Shared) -> bool {
    match state.cfg.quota {
        Some(q) => state.downloaded.load(Ordering::Relaxed) >= q,
        None => false,
    }
}

fn record_downloaded(state: &Shared, bytes: u64) {
    let total = state.downloaded.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if let Some(q) = state.cfg.quota {
        if total >= q {
            state.quota_reached.store(true, Ordering::Relaxed);
        }
    }
}

fn accept_url(cfg: &Config, url: &Url) -> bool {
    let path = url.path();
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);

    if !cfg.accept.is_empty()
        && !cfg
            .accept
            .iter()
            .any(|pat| match_glob(name, pat, cfg.ignore_case))
    {
        return false;
    }
    if cfg
        .reject
        .iter()
        .any(|pat| match_glob(name, pat, cfg.ignore_case))
    {
        return false;
    }

    let hay = url.as_str();
    if let Some(re) = &cfg.accept_regex {
        if !regex_is_match(cfg, re, hay) {
            return false;
        }
    }
    if let Some(re) = &cfg.reject_regex {
        if regex_is_match(cfg, re, hay) {
            return false;
        }
    }

    if let Some(host) = url.host_str() {
        if !cfg.domains.is_empty() && !cfg.domains.iter().any(|d| host_matches(host, d)) {
            return false;
        }
        if cfg.exclude_domains.iter().any(|d| host_matches(host, d)) {
            return false;
        }
    }

    if !cfg.include_directories.is_empty() {
        let ok = cfg.include_directories.iter().any(|d| path.starts_with(d));
        if !ok {
            return false;
        }
    }
    if cfg.exclude_directories.iter().any(|d| path.starts_with(d)) {
        return false;
    }

    true
}

fn regex_is_match(cfg: &Config, pattern: &str, hay: &str) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static PCRE_CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
        static POSIX_CACHE: RefCell<HashMap<(String, bool), posix_regex::PosixRegex<'static>>> =
            RefCell::new(HashMap::new());
    }

    if cfg.regex_type.eq_ignore_ascii_case("pcre") {
        return PCRE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let re = match cache.get(pattern) {
                Some(re) => re,
                None => {
                    let Ok(compiled) = Regex::new(pattern) else {
                        return false;
                    };
                    cache.insert(pattern.to_string(), compiled);
                    cache.get(pattern).expect("just inserted")
                }
            };
            re.is_match(hay)
        });
    }

    let key = (pattern.to_string(), cfg.ignore_case);
    POSIX_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.contains_key(&key) {
            let compiled = match posix_regex::PosixRegexBuilder::new(pattern.as_bytes())
                .with_default_classes()
                .extended(true)
                .compile()
            {
                Ok(mut re) => {
                    if cfg.ignore_case {
                        re = re.case_insensitive(true);
                    }
                    re
                }
                Err(_) => return false,
            };
            cache.insert(key.clone(), compiled);
        }
        let re = cache.get(&key).expect("just ensured");
        !re.matches(hay.as_bytes(), Some(1)).is_empty()
    })
}

fn maybe_set_xattrs(
    cfg: &Config,
    path: &Path,
    origin_url: &str,
    referrer_url: Option<&str>,
    content_type: Option<&str>,
) {
    if !cfg.xattr || path.as_os_str() == "-" {
        return;
    }
    #[cfg(unix)]
    {
        if let Err(e) = xattr::set(path, "user.xdg.origin.url", origin_url.as_bytes()) {
            eprintln!("fetchling: warning: failed to set xattr origin url: {e}");
        }
        if let Some(referrer) = referrer_url {
            if referrer != origin_url {
                if let Err(e) = xattr::set(path, "user.xdg.referrer.url", referrer.as_bytes()) {
                    eprintln!("fetchling: warning: failed to set xattr referrer url: {e}");
                }
            }
        }
        if let Some(ct) = content_type {
            let mime = ct.split(';').next().unwrap_or(ct).trim();
            if !mime.is_empty() {
                if let Err(e) = xattr::set(path, "user.mime_type", mime.as_bytes()) {
                    eprintln!("fetchling: warning: failed to set xattr mime type: {e}");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, origin_url, referrer_url, content_type);
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!("fetchling: warning: --xattr is not supported on this platform");
        });
    }
}

fn log_reject(state: &Shared, url: &Url, reason: &str) {
    state.log.info(&format!("rejected ({reason}): {url}"));
    if let Some(path) = &state.cfg.rejected_log {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{reason},{url},");
        }
    }
}

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn match_glob(name: &str, pat: &str, ignore_case: bool) -> bool {
    fetchling_core::match_glob(name, pat, ignore_case)
}

fn ftp_join_name(base: &Url, name: &str) -> Option<Url> {
    ftp_join_child(base, name, false)
}

fn ftp_join_child(base: &Url, name: &str, as_dir: bool) -> Option<Url> {
    let name = name.trim().trim_start_matches('/');
    if name.is_empty() || name.contains("://") {
        return None;
    }
    if name.contains('/') {
        return None;
    }
    let path = base.path();
    let parent = if path.ends_with('/') {
        path
    } else if let Some(i) = path.rfind('/') {
        &path[..=i]
    } else {
        "/"
    };
    let mut u = base.clone();
    if as_dir {
        u.set_path(&format!("{parent}{name}/"));
    } else {
        u.set_path(&format!("{parent}{name}"));
    }
    Some(u)
}

fn symlink_target_is_dir(target: Option<&str>) -> bool {
    target.map(|t| t.ends_with('/')).unwrap_or(false)
}

fn safe_local_symlink_target(target: &str) -> Option<&str> {
    let target = target.trim();
    if target.is_empty() || target.contains('\0') {
        return None;
    }
    if target.starts_with('/') || target.starts_with('\\') {
        return None;
    }
    if target.split(['/', '\\']).any(|p| p == "..") {
        return None;
    }
    Some(target)
}

fn safe_local_symlink_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    if name.contains('/') || name.contains('\\') {
        return None;
    }
    if name == "." || name == ".." {
        return None;
    }
    Some(name)
}

async fn enqueue_ftp_listing_entries(
    state: &Shared,
    base: &Url,
    preferred_dir: &Path,
    entries: &[FtpEntry],
    depth: i32,
) {
    if quota_exceeded(state) {
        state.quota_reached.store(true, Ordering::Relaxed);
        return;
    }
    let mut q = state.queue.lock().await;
    for entry in entries {
        match &entry.kind {
            FtpEntryKind::File => {
                let Some(child) = ftp_join_child(base, &entry.name, false) else {
                    continue;
                };
                if !ftp_child_allowed(state, base, &child) {
                    continue;
                }
                q.push_back((child, depth + 1));
            }
            FtpEntryKind::Dir => {
                let Some(child) = ftp_join_child(base, &entry.name, true) else {
                    continue;
                };
                if !ftp_child_allowed(state, base, &child) {
                    continue;
                }
                q.push_back((child, depth + 1));
            }
            FtpEntryKind::Symlink { target } => {
                if state.cfg.retr_symlinks {
                    if symlink_target_is_dir(target.as_deref()) {
                        continue;
                    }
                    let Some(child) = ftp_join_child(base, &entry.name, false) else {
                        continue;
                    };
                    if !ftp_child_allowed(state, base, &child) {
                        continue;
                    }
                    q.push_back((child, depth + 1));
                } else {
                    let Some(link_name) = safe_local_symlink_name(&entry.name) else {
                        state
                            .log
                            .info(&format!("rejected (unsafe symlink name): {}", entry.name));
                        continue;
                    };
                    let Some(target) = target.as_deref().and_then(safe_local_symlink_target) else {
                        state.log.info(&format!(
                            "rejected (unsafe symlink target): {} -> {:?}",
                            entry.name, target
                        ));
                        continue;
                    };
                    let dest = preferred_dir.join(link_name);
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    #[cfg(unix)]
                    {
                        if dest.exists() || dest.symlink_metadata().is_ok() {
                            continue;
                        }
                        if let Err(e) = std::os::unix::fs::symlink(target, &dest) {
                            state
                                .log
                                .info(&format!("failed to create symlink {}: {e}", dest.display()));
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = (dest, target);
                        state
                            .log
                            .info("FTP local symlinks require a Unix host; skipped");
                    }
                }
            }
            FtpEntryKind::Other => {}
        }
    }
}

fn ftp_child_allowed(state: &Shared, base: &Url, child: &Url) -> bool {
    if state.cfg.https_only && child.scheme() != "https" {
        log_reject(state, child, "https-only");
        return false;
    }
    if !state.cfg.span_hosts && base.host_str() != child.host_str() {
        log_reject(state, child, "span-hosts");
        return false;
    }
    if state.cfg.no_parent && !is_child_path(base, child) {
        log_reject(state, child, "no-parent");
        return false;
    }
    if state.cfg.relative_only && child.host_str() != base.host_str() {
        log_reject(state, child, "relative-only");
        return false;
    }
    if !accept_url(&state.cfg, child) {
        log_reject(state, child, "accept/reject filters");
        return false;
    }
    true
}

fn ingest_metalink_xml(
    xml: &str,
    cfg: &Config,
    urls: &mut Vec<String>,
    metalink_hashes: &mut HashMap<String, String>,
    metalink_mirrors: &mut HashMap<String, Vec<Url>>,
) -> Result<()> {
    let preferred = cfg.preferred_location.clone();
    let doc = parse_metalink_doc(xml)?;
    if cfg.metalink_index != 0 {
        if let Some(mu) = doc.select_metaurl(cfg.metalink_index) {
            urls.push(mu.url.as_str().to_string());
        } else {
            return Err(Error::Parse(format!(
                "metalink-index {} out of range ({} metaurl(s))",
                cfg.metalink_index,
                doc.metaurls.len()
            )));
        }
        return Ok(());
    }
    for f in &doc.files {
        let ordered = f.urls_ordered(preferred.as_deref());
        if ordered.is_empty() {
            continue;
        }
        let mut resolved = Vec::new();
        for u in ordered {
            let fu = normalize_url_iri(u.as_str(), cfg.iri)?;
            resolved.push(fu.url);
        }
        let primary = resolved[0].as_str().to_string();
        if !f.hashes.is_empty() {
            let encoded = encode_hashes(&f.hashes);
            for u in &resolved {
                metalink_hashes.insert(u.as_str().to_string(), encoded.clone());
            }
        }
        if resolved.len() > 1 {
            metalink_mirrors.insert(primary.clone(), resolved[1..].to_vec());
        }
        urls.push(primary);
    }
    Ok(())
}

fn is_child_path(parent: &Url, child: &Url) -> bool {
    let p = parent.path();
    let c = child.path();
    let base = if p.ends_with('/') {
        p.to_string()
    } else if let Some(i) = p.rfind('/') {
        p[..=i].to_string()
    } else {
        "/".into()
    };
    c.starts_with(&base)
}

fn fastrand() -> f64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (n % 1000) as f64 / 1000.0
}

fn verify_metalink_hashes(path: &Path, expected: &str, keep_badhash: bool) -> Result<()> {
    let hashes = if expected.contains('=') {
        decode_hashes(expected)
    } else {
        vec![MetalinkHash {
            algo: "sha-256".into(),
            hex: expected.to_string(),
        }]
    };
    if hashes.is_empty() {
        return Ok(());
    }
    let data = std::fs::read(path)?;
    for h in &hashes {
        let got = match h.algo.as_str() {
            "md5" => hex::encode(Md5::digest(&data)),
            "sha-1" => hex::encode(Sha1::digest(&data)),
            "sha-256" => hex::encode(Sha256::digest(&data)),
            "sha-512" => hex::encode(Sha512::digest(&data)),
            _ => continue,
        };
        if !got.eq_ignore_ascii_case(h.hex.trim()) {
            if !keep_badhash {
                let _ = std::fs::remove_file(path);
            }
            return Err(Error::Protocol(format!(
                "metalink {} mismatch for {}: expected {}, got {got}",
                h.algo,
                path.display(),
                h.hex
            )));
        }
    }
    Ok(())
}

fn warn_unimplemented_stubs(cfg: &Config) {
    let mut notes = Vec::new();
    if !cfg.check_certificate {
        notes.push(
            "--no-check-certificate disables TLS certificate and hostname verification; connections are not authenticated",
        );
    }
    if cfg.password.is_some()
        || cfg.http_password.is_some()
        || cfg.ftp_password.is_some()
        || cfg.proxy_password.is_some()
    {
        notes.push(
            "password options may be visible in the process list; prefer --ask-password, --use-askpass, or netrc",
        );
    }
    if cfg.random_file.is_some() || cfg.egd_file.is_some() {
        notes.push("--random-file/--egd-file are ignored (modern CSPRNGs do not need seeding)");
    }
    #[cfg(not(unix))]
    if cfg.preserve_permissions {
        notes.push("--preserve-permissions is not supported on this platform");
    }
    if cfg.use_proxy {
        let ftp_proxy_set = std::env::var("ftp_proxy")
            .or_else(|_| std::env::var("FTP_PROXY"))
            .ok()
            .filter(|s| !s.is_empty())
            .is_some();
        if ftp_proxy_set {
            notes.push(
                "FTP proxies (ftp_proxy / FTP_PROXY) are not implemented and will be ignored",
            );
        }
    }
    for n in notes {
        eprintln!("fetchling: warning: {n}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn quiet_cfg() -> Config {
        Config {
            quiet: true,
            netrc: false,
            http_keep_alive: false,
            ..Config::default()
        }
    }

    fn test_shared(cfg: Config) -> Arc<Shared> {
        let log = Logger::new(&cfg).unwrap();
        let http = HttpClient::new(&cfg, log.clone()).unwrap();
        Arc::new(Shared {
            cfg,
            log,
            http,
            ftp: FtpClient::default(),
            warc: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            visited: Mutex::new(HashSet::new()),
            link_map: Mutex::new(Vec::new()),
            robots_cache: Mutex::new(HashMap::new()),
            host_semaphores: Mutex::new(HashMap::new()),
            path_locks: Mutex::new(HashMap::new()),
            metalink_hashes: Mutex::new(HashMap::new()),
            metalink_mirrors: Mutex::new(HashMap::new()),
            sitemaps_seeded: Mutex::new(HashSet::new()),
            downloaded: AtomicU64::new(0),
            quota_reached: AtomicBool::new(false),
            worst: Mutex::new(ExitCode::Success),
        })
    }

    async fn spawn_http(replies: Vec<(u16, Vec<u8>)>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for (status, body) in replies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let n = match stream.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let mut out = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                out.extend_from_slice(&body);
                let _ = stream.write_all(&out).await;
            }
        });
        addr
    }

    fn temp_dir(kind: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fetchling-engine-{kind}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn verify_sha256_mismatch_deletes_unless_keep() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fetchling-hash-test-{}", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        let err = verify_metalink_hashes(&path, "00", false).unwrap_err();
        assert!(err.to_string().contains("mismatch"));
        assert!(!path.exists());
    }

    #[test]
    fn verify_sha256_ok() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fetchling-hash-ok-{}", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        verify_metalink_hashes(
            &path,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            false,
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_multi_hash_ok() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fetchling-hash-multi-{}", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        verify_metalink_hashes(
            &path,
            "md5=5d41402abc4b2a76b9719d911017c592,sha-256=2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            false,
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn posix_ere_plus_is_literal_without_escape_in_basic_but_special_in_extended() {
        let cfg = Config {
            regex_type: "posix".into(),
            accept_regex: Some("file.+bin".into()),
            ..Config::default()
        };
        let url = Url::parse("http://example.com/fileXbin").unwrap();
        assert!(accept_url(&cfg, &url));
        let url2 = Url::parse("http://example.com/filebin").unwrap();
        assert!(!accept_url(&cfg, &url2));
        let cfg_pcre = Config {
            regex_type: "pcre".into(),
            accept_regex: Some("file.+bin".into()),
            ..Config::default()
        };
        assert!(accept_url(&cfg_pcre, &url));
    }

    #[test]
    fn mime_type_allowed_matches() {
        assert!(mime_type_allowed(&[], Some("text/html")));
        assert!(!mime_type_allowed(&["text/html".into()], None));
        assert!(mime_type_allowed(
            &["text/html".into()],
            Some("text/html; charset=utf-8")
        ));
        assert!(mime_type_allowed(&["image/*".into()], Some("image/png")));
        assert!(mime_type_allowed(&["image".into()], Some("image/png")));
        assert!(!mime_type_allowed(
            &["text/html".into()],
            Some("application/octet-stream")
        ));
    }

    #[test]
    fn resolve_input_url_joins_base() {
        let base = Url::parse("https://example.com/dir/").unwrap();
        assert_eq!(
            resolve_input_url("file.bin", Some(&base)).unwrap(),
            "https://example.com/dir/file.bin"
        );
        assert_eq!(
            resolve_input_url("https://other/x", Some(&base)).unwrap(),
            "https://other/x"
        );
        assert_eq!(resolve_input_url("rel", None).unwrap(), "rel");
    }

    #[test]
    fn ftp_join_child_file_and_dir() {
        let base = Url::parse("ftp://example.com/pub/").unwrap();
        let file = ftp_join_child(&base, "a.bin", false).unwrap();
        assert_eq!(file.as_str(), "ftp://example.com/pub/a.bin");
        let dir = ftp_join_child(&base, "docs", true).unwrap();
        assert_eq!(dir.as_str(), "ftp://example.com/pub/docs/");
    }

    #[test]
    fn accept_url_uses_nonempty_dir_basename() {
        let cfg = Config {
            accept: vec!["docs".into()],
            ..Config::default()
        };
        let dir = Url::parse("ftp://example.com/pub/docs/").unwrap();
        assert!(accept_url(&cfg, &dir));
        let other = Url::parse("ftp://example.com/pub/other/").unwrap();
        assert!(!accept_url(&cfg, &other));
    }

    #[test]
    fn safe_symlink_target_rejects_escape() {
        assert_eq!(safe_local_symlink_target("ok.bin"), Some("ok.bin"));
        assert_eq!(safe_local_symlink_target("sub/ok.bin"), Some("sub/ok.bin"));
        assert!(safe_local_symlink_target("../escape").is_none());
        assert!(safe_local_symlink_target("/abs").is_none());
        assert!(safe_local_symlink_target("").is_none());
    }

    #[test]
    fn safe_symlink_name_rejects_traversal() {
        assert_eq!(safe_local_symlink_name("link.bin"), Some("link.bin"));
        assert!(safe_local_symlink_name("../../etc/passwd").is_none());
        assert!(safe_local_symlink_name("..").is_none());
        assert!(safe_local_symlink_name(".").is_none());
        assert!(safe_local_symlink_name("/abs").is_none());
        assert!(safe_local_symlink_name("a/b").is_none());
        assert!(safe_local_symlink_name("").is_none());
    }

    #[tokio::test]
    async fn ftp_listing_enqueue_respects_retr_symlinks() {
        let base = Url::parse("ftp://example.com/pub/").unwrap();
        let preferred = PathBuf::from("/tmp/fetchling-ftp-enq/index.html");
        let entries = vec![
            FtpEntry {
                name: "a.bin".into(),
                kind: FtpEntryKind::File,
            },
            FtpEntry {
                name: "docs".into(),
                kind: FtpEntryKind::Dir,
            },
            FtpEntry {
                name: "link.bin".into(),
                kind: FtpEntryKind::Symlink {
                    target: Some("a.bin".into()),
                },
            },
            FtpEntry {
                name: "dirlink".into(),
                kind: FtpEntryKind::Symlink {
                    target: Some("docs/".into()),
                },
            },
        ];

        let cfg = Config {
            recursive: true,
            retr_symlinks: true,
            ..Config::default()
        };
        let shared = Arc::new(Shared {
            cfg,
            log: Logger::new(&Config::default()).unwrap(),
            http: HttpClient::new(&Config::default(), Logger::new(&Config::default()).unwrap())
                .unwrap(),
            ftp: FtpClient::default(),
            warc: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            visited: Mutex::new(HashSet::new()),
            link_map: Mutex::new(Vec::new()),
            robots_cache: Mutex::new(HashMap::new()),
            host_semaphores: Mutex::new(HashMap::new()),
            path_locks: Mutex::new(HashMap::new()),
            metalink_hashes: Mutex::new(HashMap::new()),
            metalink_mirrors: Mutex::new(HashMap::new()),
            sitemaps_seeded: Mutex::new(HashSet::new()),
            downloaded: AtomicU64::new(0),
            quota_reached: AtomicBool::new(false),
            worst: Mutex::new(ExitCode::Success),
        });
        enqueue_ftp_listing_entries(&shared, &base, &preferred, &entries, 0).await;
        let q: Vec<String> = shared
            .queue
            .lock()
            .await
            .iter()
            .map(|(u, _)| u.as_str().to_string())
            .collect();
        assert!(q.contains(&"ftp://example.com/pub/a.bin".to_string()));
        assert!(q.contains(&"ftp://example.com/pub/docs/".to_string()));
        assert!(q.contains(&"ftp://example.com/pub/link.bin".to_string()));
        assert!(!q.iter().any(|u| u.contains("dirlink")));
    }

    #[test]
    fn quota_gate_trips_flag() {
        let cfg = Config {
            quota: Some(10),
            ..Config::default()
        };
        let shared = Shared {
            cfg,
            log: Logger::new(&Config::default()).unwrap(),
            http: HttpClient::new(&Config::default(), Logger::new(&Config::default()).unwrap())
                .unwrap(),
            ftp: FtpClient::default(),
            warc: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            visited: Mutex::new(HashSet::new()),
            link_map: Mutex::new(Vec::new()),
            robots_cache: Mutex::new(HashMap::new()),
            host_semaphores: Mutex::new(HashMap::new()),
            path_locks: Mutex::new(HashMap::new()),
            metalink_hashes: Mutex::new(HashMap::new()),
            metalink_mirrors: Mutex::new(HashMap::new()),
            sitemaps_seeded: Mutex::new(HashSet::new()),
            downloaded: AtomicU64::new(0),
            quota_reached: AtomicBool::new(false),
            worst: Mutex::new(ExitCode::Success),
        };
        assert!(!quota_exceeded(&shared));
        record_downloaded(&shared, 10);
        assert!(quota_exceeded(&shared));
        assert!(shared.quota_reached.load(Ordering::Relaxed));
    }

    /// Unit-test robots singleflight logic without network I/O.
    #[tokio::test]
    async fn robots_singleflight_fetches_once() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let cache: Arc<Mutex<HashMap<String, RobotsEntry>>> = Arc::new(Mutex::new(HashMap::new()));
        let host = "example.com";

        async fn ensure(
            cache: &Mutex<HashMap<String, RobotsEntry>>,
            host: &str,
            fetches: &AtomicUsize,
        ) -> Robots {
            loop {
                let wait_on = {
                    let mut c = cache.lock().await;
                    match c.get(host) {
                        Some(RobotsEntry::Ready(r)) => return r.clone(),
                        Some(RobotsEntry::Pending(n)) => Some(Arc::clone(n)),
                        None => {
                            let n = Arc::new(Notify::new());
                            c.insert(host.to_string(), RobotsEntry::Pending(Arc::clone(&n)));
                            None
                        }
                    }
                };
                if let Some(n) = wait_on {
                    n.notified().await;
                    continue;
                }
                fetches.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                let robots = Robots::default();
                let notify = {
                    let mut c = cache.lock().await;
                    let prev = c.insert(host.to_string(), RobotsEntry::Ready(robots.clone()));
                    match prev {
                        Some(RobotsEntry::Pending(n)) => n,
                        _ => Arc::new(Notify::new()),
                    }
                };
                notify.notify_waiters();
                return robots;
            }
        }

        let c1 = Arc::clone(&cache);
        let f1 = Arc::clone(&fetches);
        let c2 = Arc::clone(&cache);
        let f2 = Arc::clone(&fetches);
        let (a, b) = tokio::join!(ensure(&c1, host, &f1), ensure(&c2, host, &f2),);
        let _ = (a, b);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn path_locks_serialize_same_dest() {
        let locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(HashMap::new());
        let dest = PathBuf::from("/tmp/fetchling-path-lock-test");
        let order = Arc::new(Mutex::new(Vec::new()));

        async fn with_lock(
            locks: &Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
            dest: &Path,
            order: &Mutex<Vec<&'static str>>,
            label: &'static str,
        ) {
            let lock = {
                let mut map = locks.lock().await;
                Arc::clone(
                    map.entry(dest.to_path_buf())
                        .or_insert_with(|| Arc::new(Mutex::new(()))),
                )
            };
            let _g = lock.lock().await;
            order.lock().await.push(label);
            tokio::time::sleep(Duration::from_millis(10)).await;
            order.lock().await.push(label);
        }

        let o1 = Arc::clone(&order);
        let o2 = Arc::clone(&order);
        tokio::join!(
            with_lock(&locks, &dest, &o1, "a"),
            with_lock(&locks, &dest, &o2, "b"),
        );
        let seq = order.lock().await.clone();
        // Contiguous pairs: a,a then b,b or b,b then a,a — never interleaved a,b,a,b.
        assert!(
            seq == ["a", "a", "b", "b"] || seq == ["b", "b", "a", "a"],
            "unexpected interleaving: {seq:?}"
        );
    }

    #[test]
    fn accept_url_filters_domains_dirs_and_regex() {
        let url = Url::parse("http://www.example.com/pub/file.bin").unwrap();
        let reject = Config {
            reject: vec!["*.bin".into()],
            ..Config::default()
        };
        assert!(!accept_url(&reject, &url));
        let domains = Config {
            domains: vec!["example.com".into()],
            ..Config::default()
        };
        assert!(accept_url(&domains, &url));
        assert!(!accept_url(
            &domains,
            &Url::parse("http://other.org/file.bin").unwrap()
        ));
        let exclude = Config {
            exclude_domains: vec!["example.com".into()],
            ..Config::default()
        };
        assert!(!accept_url(&exclude, &url));
        let include = Config {
            include_directories: vec!["/pub".into()],
            ..Config::default()
        };
        assert!(accept_url(&include, &url));
        assert!(!accept_url(
            &include,
            &Url::parse("http://www.example.com/other/file.bin").unwrap()
        ));
        let exclude_dir = Config {
            exclude_directories: vec!["/pub".into()],
            ..Config::default()
        };
        assert!(!accept_url(&exclude_dir, &url));
        let case = Config {
            accept: vec!["*.BIN".into()],
            ignore_case: true,
            ..Config::default()
        };
        assert!(accept_url(&case, &url));
        let reject_re = Config {
            regex_type: "posix".into(),
            reject_regex: Some("file.+bin".into()),
            ..Config::default()
        };
        assert!(!accept_url(
            &reject_re,
            &Url::parse("http://example.com/fileXbin").unwrap()
        ));
        let bad_pcre = Config {
            regex_type: "pcre".into(),
            accept_regex: Some("[".into()),
            ..Config::default()
        };
        assert!(!accept_url(&bad_pcre, &url));
        assert!(host_matches("www.example.com", "example.com"));
        assert!(!host_matches("notexample.com", "example.com"));
    }

    #[test]
    fn mime_and_metalink_content_type_edges() {
        assert!(!mime_type_allowed(&["".into()], Some("text/html")));
        assert!(!mime_type_allowed(&["text/html".into()], Some("  ")));
        assert!(!is_metalink_content_type(None));
        assert!(is_metalink_content_type(Some("application/metalink4+xml")));
        assert!(!is_metalink_content_type(Some("text/html")));
    }

    #[test]
    fn ftp_join_rejects_and_strips_file_parent() {
        let base = Url::parse("ftp://example.com/pub/").unwrap();
        assert!(ftp_join_child(&base, "", false).is_none());
        assert!(ftp_join_child(&base, "ftp://x", false).is_none());
        assert!(ftp_join_child(&base, "a/b", false).is_none());
        assert_eq!(
            ftp_join_name(&base, "a.bin").unwrap().as_str(),
            ftp_join_child(&base, "a.bin", false).unwrap().as_str()
        );
        let file_base = Url::parse("ftp://example.com/pub/file").unwrap();
        assert_eq!(
            ftp_join_child(&file_base, "a.bin", false).unwrap().as_str(),
            "ftp://example.com/pub/a.bin"
        );
    }

    #[test]
    fn child_path_and_symlink_dir_flag() {
        let parent = Url::parse("http://example.com/a/").unwrap();
        assert!(is_child_path(
            &parent,
            &Url::parse("http://example.com/a/b").unwrap()
        ));
        assert!(!is_child_path(
            &parent,
            &Url::parse("http://example.com/b").unwrap()
        ));
        assert!(symlink_target_is_dir(Some("docs/")));
        assert!(!symlink_target_is_dir(None));
        assert!(!symlink_target_is_dir(Some("a.bin")));
    }

    #[test]
    fn ingest_metalink_xml_urls_hashes_mirrors_and_index() {
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="f.bin">
            <url>http://a.example.com/f.bin</url>
            <url>http://b.example.com/f.bin</url>
            <hash type="sha-256">abc</hash>
          </file>
          <metaurl mediatype="application/metalink4+xml">http://example.com/a.meta4</metaurl>
          <metaurl mediatype="application/metalink4+xml">http://example.com/b.meta4</metaurl>
        </metalink>"#;
        let mut urls = Vec::new();
        let mut hashes = HashMap::new();
        let mut mirrors = HashMap::new();
        ingest_metalink_xml(
            xml,
            &Config::default(),
            &mut urls,
            &mut hashes,
            &mut mirrors,
        )
        .unwrap();
        assert_eq!(urls, vec!["http://a.example.com/f.bin".to_string()]);
        assert_eq!(
            hashes.get("http://a.example.com/f.bin").map(String::as_str),
            Some("sha-256=abc")
        );
        assert_eq!(
            hashes.get("http://b.example.com/f.bin").map(String::as_str),
            Some("sha-256=abc")
        );
        assert_eq!(
            mirrors
                .get("http://a.example.com/f.bin")
                .map(|v| v.iter().map(|u| u.as_str().to_string()).collect::<Vec<_>>()),
            Some(vec!["http://b.example.com/f.bin".to_string()])
        );
        let idx = Config {
            metalink_index: 2,
            ..Config::default()
        };
        let mut urls = Vec::new();
        ingest_metalink_xml(
            xml,
            &idx,
            &mut urls,
            &mut HashMap::new(),
            &mut HashMap::new(),
        )
        .unwrap();
        assert_eq!(urls, vec!["http://example.com/b.meta4".to_string()]);
        let bad = Config {
            metalink_index: 9,
            ..Config::default()
        };
        let err = ingest_metalink_xml(
            xml,
            &bad,
            &mut Vec::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[test]
    fn verify_keep_badhash_unknown_algo_and_sha512() {
        let dir = temp_dir("hash");
        let path = dir.join("hello.bin");
        std::fs::write(&path, b"hello").unwrap();
        let err = verify_metalink_hashes(&path, "00", true).unwrap_err();
        assert!(err.to_string().contains("mismatch"));
        assert!(path.exists());
        verify_metalink_hashes(&path, "blake2=abc", false).unwrap();
        verify_metalink_hashes(
            &path,
            "sha-512=9b71d224bd62f3785d96d46ad3ea3d73319bfbc2890caadae2dff72519673ca72323c3d99ba5c11d7c7acc6e14b8c5da0c4663475c2e5c3adef46f73bcdec043",
            false,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_run_parse_errors_without_network() {
        let cfg = quiet_cfg();
        let err = Engine::new(cfg).unwrap().run().await.unwrap_err();
        assert!(matches!(err, Error::Parse(msg) if msg.contains("no URL specified")));
        let mut cfg = quiet_cfg();
        cfg.base = Some("not a url".into());
        cfg.urls = vec!["http://example.com/x".into()];
        let err = Engine::new(cfg).unwrap().run().await.unwrap_err();
        assert!(matches!(err, Error::Parse(msg) if msg.contains("bad --base")));
    }

    #[tokio::test]
    async fn engine_run_saves_200_body() {
        let addr = spawn_http(vec![(200, b"hello".to_vec())]).await;
        let dir = temp_dir("run200");
        let mut cfg = quiet_cfg();
        cfg.directory_prefix = dir.display().to_string();
        cfg.urls = vec![format!("http://{addr}/file.bin")];
        let code = Engine::new(cfg).unwrap().run().await.unwrap();
        assert_eq!(code, ExitCode::Success);
        assert_eq!(std::fs::read(dir.join("file.bin")).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_run_robots_deny_skips_page() {
        let addr = spawn_http(vec![(200, b"User-agent: *\nDisallow: /\n".to_vec())]).await;
        let dir = temp_dir("robots");
        let mut cfg = quiet_cfg();
        cfg.recursive = true;
        cfg.directory_prefix = dir.display().to_string();
        cfg.urls = vec![format!("http://{addr}/page.bin")];
        let code = Engine::new(cfg).unwrap().run().await.unwrap();
        assert_eq!(code, ExitCode::Success);
        let dest = local_path_for_url(
            &Config {
                recursive: true,
                directory_prefix: dir.display().to_string(),
                ..Config::default()
            },
            &Url::parse(&format!("http://{addr}/page.bin")).unwrap(),
        );
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn apply_and_enqueue_metalink() {
        let base = Url::parse("http://example.com/dir/file.bin").unwrap();
        let described = test_shared(quiet_cfg());
        apply_metalink_links(
            &described,
            &base,
            &["<f.meta4>; rel=describedby; type=\"application/metalink4+xml\"".into()],
        )
        .await
        .unwrap();
        let q = described.queue.lock().await;
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].0.as_str(), "http://example.com/dir/f.meta4");
        drop(q);

        let dup = test_shared(quiet_cfg());
        apply_metalink_links(
            &dup,
            &base,
            &[
                "<http://mirror.example.com/f.bin>; rel=duplicate; pri=1; digest=SHA-256=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
                "<http://other.example.com/f.bin>; rel=duplicate; pri=2".into(),
            ],
        )
        .await
        .unwrap();
        let q = dup.queue.lock().await;
        assert_eq!(q[0].0.as_str(), "http://mirror.example.com/f.bin");
        drop(q);
        let mirrors = dup.metalink_mirrors.lock().await;
        assert_eq!(
            mirrors
                .get("http://mirror.example.com/f.bin")
                .map(|v| v.iter().map(|u| u.as_str().to_string()).collect::<Vec<_>>()),
            Some(vec!["http://other.example.com/f.bin".to_string()])
        );
        drop(mirrors);
        let hashes = dup.metalink_hashes.lock().await;
        assert_eq!(
            hashes
                .get("http://mirror.example.com/f.bin")
                .map(String::as_str),
            Some("sha-256=0000000000000000000000000000000000000000000000000000000000000000")
        );

        let dir = temp_dir("metafile");
        let path = dir.join("f.meta4");
        std::fs::write(
            &path,
            r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="f.bin">
            <url>http://a.example.com/f.bin</url>
            <url>http://b.example.com/f.bin</url>
            <hash type="sha-256">abc</hash>
          </file>
        </metalink>"#,
        )
        .unwrap();
        let shared = test_shared(quiet_cfg());
        enqueue_metalink_file(&shared, &path).await.unwrap();
        let q = shared.queue.lock().await;
        assert_eq!(q[0].0.as_str(), "http://a.example.com/f.bin");
        drop(q);
        assert_eq!(
            shared
                .metalink_hashes
                .lock()
                .await
                .get("http://b.example.com/f.bin")
                .map(String::as_str),
            Some("sha-256=abc")
        );
        assert_eq!(
            shared
                .metalink_mirrors
                .lock()
                .await
                .get("http://a.example.com/f.bin")
                .map(|v| v.len()),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ftp_child_allowed_and_log_reject() {
        let mut https = quiet_cfg();
        https.https_only = true;
        let shared = test_shared(https);
        let base = Url::parse("ftp://example.com/pub/").unwrap();
        let child = Url::parse("ftp://example.com/pub/a.bin").unwrap();
        assert!(!ftp_child_allowed(&shared, &base, &child));

        let mut noparent = quiet_cfg();
        noparent.no_parent = true;
        let shared = test_shared(noparent);
        let dir_base = Url::parse("http://example.com/a/").unwrap();
        assert!(!ftp_child_allowed(
            &shared,
            &dir_base,
            &Url::parse("http://example.com/b").unwrap()
        ));
        assert!(ftp_child_allowed(
            &shared,
            &dir_base,
            &Url::parse("http://example.com/a/c").unwrap()
        ));

        let dir = temp_dir("reject");
        let log_path = dir.join("rejected.log");
        let mut cfg = quiet_cfg();
        cfg.rejected_log = Some(log_path.display().to_string());
        let shared = test_shared(cfg);
        let url = Url::parse("http://example.com/x").unwrap();
        log_reject(&shared, &url, "robots.txt");
        let body = std::fs::read_to_string(&log_path).unwrap();
        assert!(body.contains("robots.txt,http://example.com/x,"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ftp_listing_creates_local_symlink() {
        let dir = temp_dir("ftplink");
        let base = Url::parse("ftp://example.com/pub/").unwrap();
        let entries = vec![
            FtpEntry {
                name: "link.bin".into(),
                kind: FtpEntryKind::Symlink {
                    target: Some("a.bin".into()),
                },
            },
            FtpEntry {
                name: "../escape".into(),
                kind: FtpEntryKind::Symlink {
                    target: Some("a.bin".into()),
                },
            },
            FtpEntry {
                name: "skip".into(),
                kind: FtpEntryKind::Other,
            },
        ];
        let cfg = Config {
            retr_symlinks: false,
            quiet: true,
            netrc: false,
            ..Config::default()
        };
        let shared = test_shared(cfg);
        enqueue_ftp_listing_entries(&shared, &base, &dir, &entries, 0).await;
        assert!(shared.queue.lock().await.is_empty());
        let dest = dir.join("link.bin");
        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(meta.file_type().is_symlink());
        assert!(!dir.join("escape").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
