//! Options that fetchling rejects until implemented.

pub fn is_deferred_option(name: &str) -> bool {
    let name = name.strip_prefix("no-").unwrap_or(name);
    DEFERRED.binary_search(&name).is_ok()
}

/// Sorted list of deferred long options (canonical, without `no-` prefix).
pub static DEFERRED: &[&str] = &[
    "bind-interface",
    "check-hostname",
    "chunk-size",
    "cookie-suffixes",
    "dane",
    "default-http-port",
    "default-https-port",
    "dns-cache-preload",
    "download-attr",
    "filter-urls",
    "fsync-policy",
    "gnupg-homedir",
    "hpkp",
    "hpkp-file",
    "hsts-preload",
    "hsts-preload-file",
    "http2",
    "http2-only",
    "http2-request-window",
    "https-enforce",
    "hyperlink",
    "input-encoding",
    "keep-extension",
    "list-plugins",
    "local-db",
    "local-plugin",
    "metalink",
    "n",
    "ocsp",
    "ocsp-date",
    "ocsp-file",
    "ocsp-nonce",
    "ocsp-server",
    "ocsp-stapling",
    "plugin",
    "plugin-dirs",
    "plugin-help",
    "plugin-opt",
    "save-content-on",
    "signature-extensions",
    "stats-dns",
    "stats-ocsp",
    "stats-server",
    "stats-site",
    "stats-tls",
    "tcp-fastopen",
    "tls-false-start",
    "tls-resume",
    "tls-session-file",
    "verify-save-failed",
    "verify-sig",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_sorted() {
        let mut v = DEFERRED.to_vec();
        v.sort_unstable();
        assert_eq!(v, DEFERRED);
    }

    #[test]
    fn detects_http2() {
        assert!(is_deferred_option("http2"));
        assert!(is_deferred_option("no-http2"));
        assert!(!is_deferred_option("continue"));
        assert!(!is_deferred_option("force-css"));
        assert!(!is_deferred_option("filter-mime-type"));
        assert!(!is_deferred_option("http-proxy"));
        assert!(!is_deferred_option("force-metalink"));
        assert!(!is_deferred_option("cut-url-get-vars"));
    }
}
