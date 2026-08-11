use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Config, Error, Result};

const BAR_WIDTH: usize = 20;
const DRAW_INTERVAL: Duration = Duration::from_millis(100);
const DOT_BYTES: u64 = 1024;
const DOTS_PER_LINE: u32 = 50;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Clone)]
pub struct Logger {
    verbose: bool,
    debug: bool,
    server_response: bool,
    is_tty: bool,
    file: Option<Arc<Mutex<std::fs::File>>>,
}

impl Logger {
    pub fn new(cfg: &Config) -> Result<Self> {
        let file = if let Some(path) = &cfg.logfile {
            let f = OpenOptions::new()
                .create(true)
                .write(true)
                .append(cfg.append_output)
                .truncate(!cfg.append_output)
                .open(path)
                .map_err(|e| {
                    Error::Io(std::io::Error::new(
                        e.kind(),
                        format!("cannot open logfile {path}: {e}"),
                    ))
                })?;
            Some(Arc::new(Mutex::new(f)))
        } else {
            None
        };

        Ok(Self {
            verbose: cfg.verbose && !cfg.quiet,
            debug: cfg.debug,
            server_response: cfg.server_response,
            is_tty: io::stderr().is_terminal(),
            file,
        })
    }

    fn write_line(&self, to_stderr: bool, msg: &str) {
        if to_stderr {
            let _ = writeln!(io::stderr(), "{msg}");
        }
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{msg}");
            }
        }
    }

    /// Verbose status for both TTY and pipes (`saved`, retries, …).
    pub fn info(&self, msg: &str) {
        if self.verbose {
            self.write_line(true, msg);
        }
    }

    /// Human download story (URL / redirects / saving-as). TTY + verbose only.
    pub fn narrative(&self, msg: &str) {
        if narrative_enabled(self.verbose, self.is_tty) {
            self.write_line(true, msg);
        }
    }

    /// Errors always go to stderr (script-friendly). Also mirrored to logfile when set.
    pub fn error(&self, msg: &str) {
        self.write_line(true, msg);
    }

    pub fn debug(&self, msg: &str) {
        if self.debug {
            self.write_line(true, &format!("DEBUG: {msg}"));
        }
    }

    pub fn server(&self, msg: &str) {
        if self.server_response || self.debug {
            self.write_line(true, msg);
        }
    }
}

pub fn narrative_enabled(verbose: bool, is_tty: bool) -> bool {
    verbose && is_tty
}

pub fn format_fetch_start(url: &str) -> String {
    redact_url_for_log(url)
}

/// Single redirect hop: `  -> {url}`.
pub fn format_redirect_hop(url: &str) -> String {
    format!("  -> {}", redact_url_for_log(url))
}

/// Collapsed redirect follow-up; `None` when there were no hops.
pub fn format_redirects(hops: &[String]) -> Option<String> {
    match hops.len() {
        0 => None,
        1 => Some(format_redirect_hop(&hops[0])),
        n => Some(format!(
            "  -> {n} redirects, final {}",
            redact_url_for_log(&hops[n - 1])
        )),
    }
}

/// Strip URL userinfo so passwords never appear in narrative logs.
pub fn redact_url_for_log(url: &str) -> String {
    let Ok(mut u) = url::Url::parse(url) else {
        return url.to_string();
    };
    if !u.username().is_empty() || u.password().is_some() {
        let _ = u.set_username("");
        let _ = u.set_password(None);
    }
    u.as_str().to_string()
}

/// DNS resolution line: `  dns host  ip, ip, …` (addresses without ports).
pub fn format_dns(host: &str, addrs: &[std::net::SocketAddr]) -> String {
    let ips: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
    format!("  dns {host}  {}", ips.join(", "))
}

pub fn format_connected(addr: std::net::SocketAddr) -> String {
    format!("  connected {addr}")
}

pub fn format_reuse(host: &str, port: u16) -> String {
    format!("  reusing connection {host}:{port}")
}

pub fn format_http_status(code: u16, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("  HTTP {code} {r}"),
        None => format!("  HTTP {code}"),
    }
}

/// Length / content-type line; `None` when neither is known.
///
/// When `already` is set (resume), appends `, R (human) remaining`.
pub fn format_length(bytes: Option<u64>, content_type: Option<&str>) -> Option<String> {
    format_length_detail(bytes, None, content_type)
}

/// Length line with optional resume offset (`already` bytes already on disk).
pub fn format_length_detail(
    total: Option<u64>,
    already: Option<u64>,
    content_type: Option<&str>,
) -> Option<String> {
    let ct = content_type.map(str::trim).filter(|s| !s.is_empty());
    let remaining = match (total, already) {
        (Some(t), Some(a)) if a > 0 && a < t => Some(t - a),
        _ => None,
    };

    let mut core = match total {
        Some(n) => {
            let mut s = format!("  length {n} ({})", format_bytes(n));
            if let Some(r) = remaining {
                s.push_str(&format!(", {r} ({}) remaining", format_bytes(r)));
            }
            s
        }
        None => {
            if ct.is_none() && remaining.is_none() {
                return None;
            }
            String::from("  length")
        }
    };
    if let Some(ct) = ct {
        if total.is_none() && remaining.is_none() {
            return Some(format!("  type {ct}"));
        }
        core.push_str("  ");
        core.push_str(ct);
    }
    Some(core)
}

/// Destination line before the progress bar (name only; size is on `length`).
pub fn format_saving_as(label: &str) -> String {
    format!("  saving as {label}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressStyle {
    Bar,
    Dot,
}

/// Whether a progress indicator should be drawn on stderr.
///
/// `bar` is TTY-only by default; `--show-progress` forces it when piped.
/// `dot` works without a TTY (log-style output). Quiet always disables.
pub fn progress_enabled(quiet: bool, show_progress: bool, style: &str, is_tty: bool) -> bool {
    if quiet {
        return false;
    }
    if style.eq_ignore_ascii_case("dot") {
        return true;
    }
    if show_progress {
        return true;
    }
    style.eq_ignore_ascii_case("bar") && is_tty
}

/// Basename (or `stdout`) used as the progress label.
pub fn dest_label(dest: &Path) -> String {
    if dest.as_os_str() == "-" {
        "stdout".into()
    } else {
        dest.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
            .to_string()
    }
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n}B")
    } else if value >= 100.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.2}{}", UNITS[unit])
    }
}

pub fn format_rate(bytes: u64, elapsed: Duration, bits: bool) -> String {
    let secs = elapsed.as_secs_f64().max(1e-6);
    let per_sec = bytes as f64 / secs;
    if bits {
        let bits_per_sec = per_sec * 8.0;
        format!("{}/s", format_bits(bits_per_sec))
    } else {
        format!("{}/s", format_bytes(per_sec as u64))
    }
}

fn format_bits(bits_per_sec: f64) -> String {
    const UNITS: &[&str] = &["b", "Kb", "Mb", "Gb", "Tb"];
    let mut value = bits_per_sec;
    let mut unit = 0usize;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.2}{}", UNITS[unit])
    }
}

pub fn format_eta(remaining_bytes: u64, bytes_per_sec: f64) -> Option<String> {
    if bytes_per_sec < 1.0 {
        return None;
    }
    let secs = (remaining_bytes as f64 / bytes_per_sec).ceil() as u64;
    if secs < 60 {
        Some(format!("eta {secs}s"))
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        Some(format!("eta {m}m{s}s"))
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        Some(format!("eta {h}h{m}m"))
    }
}

pub fn render_bar(pct: u64, width: usize) -> String {
    render_bar_parts(pct.min(100), 0, 100, width)
}

/// Progress bar with resume shading: prior bytes (`▓`), this session (`█`), rest (`░`).
pub fn render_bar_parts(current: u64, initial: u64, total: u64, width: usize) -> String {
    let width = width.max(1);
    let mut out = String::with_capacity(width + 2);
    out.push('[');
    if total == 0 {
        for _ in 0..width {
            out.push('░');
        }
        out.push(']');
        return out;
    }
    let prior = ((initial.min(total) as u128 * width as u128) / total as u128) as usize;
    let filled = ((current.min(total) as u128 * width as u128) / total as u128) as usize;
    let prior = prior.min(width);
    let filled = filled.max(prior).min(width);
    for i in 0..width {
        if i < prior {
            out.push('▓');
        } else if i < filled {
            out.push('█');
        } else {
            out.push('░');
        }
    }
    out.push(']');
    out
}

pub struct ProgressBar {
    enabled: bool,
    style: ProgressStyle,
    label: String,
    total: Option<u64>,
    current: u64,
    /// Bytes already on disk before this transfer (resume); rate uses session delta.
    initial: u64,
    started_at: Instant,
    last_draw: Instant,
    report_bits: bool,
    is_tty: bool,
    /// Bytes already accounted for as printed dots.
    dot_bytes_printed: u64,
    dot_col: u32,
    last_pct: u64,
}

impl ProgressBar {
    pub fn new(cfg: &Config, total: Option<u64>, label: impl Into<String>) -> Self {
        Self::with_initial(cfg, total, 0, label)
    }

    /// Progress starting at `initial` (e.g. resume offset). Percent uses absolute
    /// `current/total`; rate/ETA use bytes transferred this session.
    pub fn with_initial(
        cfg: &Config,
        total: Option<u64>,
        initial: u64,
        label: impl Into<String>,
    ) -> Self {
        let is_tty = io::stderr().is_terminal();
        let style = if cfg.progress.eq_ignore_ascii_case("dot") {
            ProgressStyle::Dot
        } else {
            ProgressStyle::Bar
        };
        let enabled = progress_enabled(cfg.quiet, cfg.show_progress, &cfg.progress, is_tty);
        let now = Instant::now();
        Self {
            enabled,
            style,
            label: label.into(),
            total,
            current: initial,
            initial,
            started_at: now,
            last_draw: now.checked_sub(DRAW_INTERVAL).unwrap_or(now),
            report_bits: cfg.report_speed_bits,
            is_tty,
            dot_bytes_printed: initial,
            dot_col: 0,
            last_pct: u64::MAX,
        }
    }

    pub fn update(&mut self, n: u64) {
        self.current = self.current.saturating_add(n);
        if !self.enabled {
            return;
        }
        match self.style {
            ProgressStyle::Dot => self.draw_dots(),
            ProgressStyle::Bar => {
                let now = Instant::now();
                let pct = self.percent().unwrap_or(0);
                let due =
                    now.duration_since(self.last_draw) >= DRAW_INTERVAL || pct != self.last_pct;
                if due {
                    self.draw_bar_line();
                    self.last_draw = now;
                    self.last_pct = pct;
                }
            }
        }
    }

    pub fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        match self.style {
            ProgressStyle::Bar => {
                self.draw_bar_line();
                // Clear remainder of the line then newline for following status.
                let _ = write!(io::stderr(), "\r\x1b[K");
                let _ = writeln!(io::stderr());
            }
            ProgressStyle::Dot => {
                if self.dot_col > 0 {
                    let _ = writeln!(io::stderr());
                }
            }
        }
        let _ = io::stderr().flush();
    }

    fn session_bytes(&self) -> u64 {
        self.current.saturating_sub(self.initial)
    }

    fn percent(&self) -> Option<u64> {
        let total = self.total.filter(|t| *t > 0)?;
        Some(
            self.current
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(0)
                .min(100),
        )
    }

    fn rate_bps(&self) -> f64 {
        let secs = self.started_at.elapsed().as_secs_f64().max(1e-6);
        self.session_bytes() as f64 / secs
    }

    fn draw_bar_line(&self) {
        let elapsed = self.started_at.elapsed();
        let rate = format_rate(self.session_bytes(), elapsed, self.report_bits);
        let line = if let Some(total) = self.total.filter(|t| *t > 0) {
            let pct = self.percent().unwrap_or(0);
            let bar = render_bar_parts(self.current, self.initial, total, BAR_WIDTH);
            let mut s = format!(
                "{}  {}  {:>3}%  {} / {}  {}",
                self.label,
                bar,
                pct,
                format_bytes(self.current),
                format_bytes(total),
                rate
            );
            if let Some(eta) = format_eta(total.saturating_sub(self.current), self.rate_bps()) {
                s.push(' ');
                s.push_str(&eta);
            }
            s
        } else {
            let frame = SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()];
            format!(
                "{}  {}  {}  {}",
                self.label,
                frame,
                format_bytes(self.current),
                rate
            )
        };

        if self.is_tty {
            let _ = write!(io::stderr(), "\r{line}\x1b[K");
        } else {
            let _ = write!(io::stderr(), "\r{line}");
        }
        let _ = io::stderr().flush();
    }

    fn draw_dots(&mut self) {
        while self.current >= self.dot_bytes_printed.saturating_add(DOT_BYTES) {
            let _ = write!(io::stderr(), ".");
            self.dot_bytes_printed = self.dot_bytes_printed.saturating_add(DOT_BYTES);
            self.dot_col += 1;
            if self.dot_col >= DOTS_PER_LINE {
                let _ = writeln!(io::stderr());
                self.dot_col = 0;
            }
        }
        let _ = io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn quiet_disables_progress() {
        assert!(!progress_enabled(true, true, "bar", true));
        assert!(!progress_enabled(true, false, "bar", true));
        assert!(!progress_enabled(true, false, "dot", false));
    }

    #[test]
    fn tty_enables_bar_by_default() {
        assert!(progress_enabled(false, false, "bar", true));
        assert!(!progress_enabled(false, false, "bar", false));
    }

    #[test]
    fn dot_works_without_tty() {
        assert!(progress_enabled(false, false, "dot", false));
        assert!(progress_enabled(false, false, "dot", true));
    }

    #[test]
    fn show_progress_forces_non_tty_bar() {
        assert!(progress_enabled(false, true, "bar", false));
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(500), "500B");
        assert_eq!(format_bytes(1536), "1.50KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00MB");
        assert_eq!(format_bytes(100 * 1024 * 1024), "100MB");
    }

    #[test]
    fn format_rate_bytes_and_bits() {
        let d = Duration::from_secs(1);
        assert_eq!(format_rate(1024 * 1024, d, false), "1.00MB/s");
        assert!(
            format_rate(1_000_000, d, true).ends_with("Mb/s")
                || format_rate(1_000_000, d, true).contains("Mb")
        );
    }

    #[test]
    fn format_eta_values() {
        assert_eq!(format_eta(900, 100.0).as_deref(), Some("eta 9s"));
        assert_eq!(format_eta(130, 1.0).as_deref(), Some("eta 2m10s"));
        assert!(format_eta(100, 0.0).is_none());
    }

    #[test]
    fn render_bar_fill() {
        assert_eq!(render_bar(0, 4), "[░░░░]");
        assert_eq!(render_bar(50, 4), "[██░░]");
        assert_eq!(render_bar(100, 4), "[████]");
    }

    #[test]
    fn render_bar_parts_shows_prior_and_session() {
        // 25% prior, 50% current of total → 1 prior + 1 session on width 4
        assert_eq!(render_bar_parts(50, 25, 100, 4), "[▓█░░]");
        assert_eq!(render_bar_parts(50, 0, 100, 4), "[██░░]");
        assert_eq!(render_bar_parts(50, 50, 100, 4), "[▓▓░░]");
    }

    #[test]
    fn dest_label_stdout_and_file() {
        assert_eq!(dest_label(Path::new("-")), "stdout");
        assert_eq!(dest_label(Path::new("/tmp/a/file.bin")), "file.bin");
    }

    #[test]
    fn narrative_requires_verbose_and_tty() {
        assert!(narrative_enabled(true, true));
        assert!(!narrative_enabled(true, false));
        assert!(!narrative_enabled(false, true));
        assert!(!narrative_enabled(false, false));
    }

    #[test]
    fn format_fetch_start_is_url() {
        assert_eq!(
            format_fetch_start("https://example.com/a"),
            "https://example.com/a"
        );
    }

    #[test]
    fn redact_url_strips_userinfo() {
        assert_eq!(
            redact_url_for_log("https://user:secret@example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            format_fetch_start("https://user:secret@example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(format_redirect_hop("ftp://u:p@host/x"), "  -> ftp://host/x");
    }

    #[test]
    fn format_redirects_collapses() {
        assert!(format_redirects(&[]).is_none());
        assert_eq!(
            format_redirects(&["https://a.example/x".into()]).as_deref(),
            Some("  -> https://a.example/x")
        );
        assert_eq!(
            format_redirects(&[
                "https://a.example/1".into(),
                "https://b.example/2".into(),
                "https://c.example/3".into(),
            ])
            .as_deref(),
            Some("  -> 3 redirects, final https://c.example/3")
        );
    }

    #[test]
    fn format_redirect_hop_line() {
        assert_eq!(
            format_redirect_hop("https://b.example/x"),
            "  -> https://b.example/x"
        );
    }

    #[test]
    fn format_dns_lists_ips_without_ports() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
        let addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 443),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443),
        ];
        assert_eq!(
            format_dns("example.com", &addrs),
            "  dns example.com  1.2.3.4, ::1"
        );
    }

    #[test]
    fn format_connected_and_reuse() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)), 443);
        assert_eq!(format_connected(addr), "  connected 9.9.9.9:443");
        assert_eq!(
            format_reuse("example.com", 443),
            "  reusing connection example.com:443"
        );
    }

    #[test]
    fn format_http_status_optional_reason() {
        assert_eq!(format_http_status(200, None), "  HTTP 200");
        assert_eq!(
            format_http_status(302, Some("Moved Permanently")),
            "  HTTP 302 Moved Permanently"
        );
        assert_eq!(format_http_status(200, Some("  ")), "  HTTP 200");
    }

    #[test]
    fn format_length_variants() {
        assert!(format_length(None, None).is_none());
        assert_eq!(
            format_length(Some(1024), None).as_deref(),
            Some("  length 1024 (1.00KB)")
        );
        assert_eq!(
            format_length(None, Some("text/plain")).as_deref(),
            Some("  type text/plain")
        );
        assert_eq!(
            format_length(Some(1024), Some("text/plain")).as_deref(),
            Some("  length 1024 (1.00KB)  text/plain")
        );
    }

    #[test]
    fn format_length_detail_shows_remaining_on_resume() {
        assert_eq!(
            format_length_detail(Some(1000), Some(400), Some("application/octet-stream"))
                .as_deref(),
            Some("  length 1000 (1000B), 600 (600B) remaining  application/octet-stream")
        );
        assert_eq!(
            format_length_detail(Some(1000), Some(0), None).as_deref(),
            Some("  length 1000 (1000B)")
        );
    }

    #[test]
    fn format_saving_as_name_only() {
        assert_eq!(format_saving_as("file.bin"), "  saving as file.bin");
    }
}
