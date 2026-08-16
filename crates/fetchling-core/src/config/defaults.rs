//! Full runtime configuration for fetchling (CLI + wgetrc defaults).

/// Full fetchling configuration.
///
/// Field defaults follow wgetrc semantics unless noted. Set fields directly in
/// library code; parenthetical CLI / wget names in field docs are compatibility
/// aliases (for example `quiet` ↔ `--quiet`), not a requirement to use the CLI.
#[derive(Debug, Clone)]
pub struct Config {
    /// Go to background after startup (`--background` / `-b`).
    pub background: bool,
    /// Log messages to this file (`--output-file` / `-o`).
    pub logfile: Option<String>,
    /// Append to logfile instead of truncating (`--append-output` / `-a`).
    pub append_output: bool,
    /// Print debugging information (`--debug` / `-d`).
    pub debug: bool,
    /// Suppress output (`--quiet` / `-q`).
    pub quiet: bool,
    /// Verbose status output (default true; `--verbose` / `-v`).
    pub verbose: bool,
    /// Report bandwidth in bits/s (`--report-speed=bits`).
    pub report_speed_bits: bool,
    /// Download URLs listed in this file (`--input-file` / `-i`).
    pub input_file: Option<String>,
    /// Metalink input file (`--input-metalink`).
    pub input_metalink: Option<String>,
    /// Treat input file as HTML (`--force-html` / `-F`).
    pub force_html: bool,
    /// Treat input file as CSS (`--force-css`).
    pub force_css: bool,
    /// Treat input file as Metalink (`--force-metalink`).
    pub force_metalink: bool,
    /// Treat input file as RSS (`--force-rss`).
    pub force_rss: bool,
    /// Treat input file as Atom (`--force-atom`).
    pub force_atom: bool,
    /// Treat input file as a sitemap (`--force-sitemap`).
    pub force_sitemap: bool,
    /// Follow sitemaps when recursing (`--follow-sitemaps`).
    pub follow_sitemaps: bool,
    /// Base URL for resolving relative links (`--base` / `-B`).
    pub base: Option<String>,
    /// Alternate config file path (`--config`).
    pub config_file: Option<String>,
    /// Do not read any config file (`--no-config`).
    pub no_config: bool,
    /// Log URL rejection reasons to this file (`--rejected-log`).
    pub rejected_log: Option<String>,
    /// wgetrc-style commands from `--execute` / `-e`.
    pub execute_commands: Vec<String>,

    /// Bind local sockets to this address (`--bind-address`).
    pub bind_address: Option<String>,
    /// Bind DNS requests to this address (`--bind-dns-address`).
    pub bind_dns_address: Option<String>,
    /// Override DNS servers (`--dns-servers`).
    pub dns_servers: Option<String>,
    /// Retry count; `0` means unlimited (`--tries` / `-t`).
    pub tries: u32,
    /// Write document to this path, or `-` for stdout (`--output-document` / `-O`).
    pub output_document: Option<String>,
    /// Skip downloads that would overwrite existing files (`--no-clobber` / `-nc`).
    pub no_clobber: bool,
    /// Fetchling extension: auto-rename to `file.1`, `file.2`, … on collision.
    /// When false (default), overwrite on collision.
    pub unique_names: bool,
    /// Rotate numbered backups before overwriting (`--backups`).
    pub backups: u32,
    /// Resume a partially downloaded file (`--continue` / `-c`).
    pub continue_download: bool,
    /// Start downloading at this zero-based byte offset (`--start-pos`).
    pub start_pos: Option<u64>,
    /// Progress indicator type: `bar` or `dot` (`--progress`).
    pub progress: String,
    /// Force progress display even when not a TTY (`--show-progress`).
    pub show_progress: bool,
    /// Skip re-retrieve unless remote is newer (`--timestamping` / `-N`).
    pub timestamping: bool,
    /// Send `If-Modified-Since` in timestamping mode (default true).
    pub if_modified_since: bool,
    /// Set local file mtime from server (default true).
    pub use_server_timestamps: bool,
    /// Print server response headers (`--server-response` / `-S`).
    pub server_response: bool,
    /// Do not download; check existence only (`--spider`).
    pub spider: bool,
    /// Combined timeout applied by [`Config::apply_timeout`] (`--timeout` / `-T`).
    pub timeout: Option<f64>,
    /// DNS lookup timeout in seconds (`--dns-timeout`).
    pub dns_timeout: Option<f64>,
    /// Connect timeout in seconds (`--connect-timeout`).
    pub connect_timeout: Option<f64>,
    /// Read timeout in seconds (`--read-timeout`).
    pub read_timeout: Option<f64>,
    /// Bandwidth limit in bytes/sec (`--limit-rate`).
    pub limit_rate: Option<u64>,
    /// Wait between retrievals in seconds (`--wait`).
    pub wait: f64,
    /// Wait between retries of failed downloads (`--waitretry`).
    pub waitretry: f64,
    /// Randomize wait duration (`--random-wait`).
    pub random_wait: bool,
    /// Honor proxy environment / settings (default true; `--no-proxy` clears).
    pub use_proxy: bool,
    /// HTTP proxy URL (`http_proxy` / `--proxy` family).
    pub http_proxy: Option<String>,
    /// HTTPS proxy URL.
    pub https_proxy: Option<String>,
    /// Download quota in bytes (`--quota`).
    pub quota: Option<u64>,
    /// Cache DNS lookups (default true).
    pub dns_cache: bool,
    /// Filename restriction modes (`--restrict-file-names`).
    pub restrict_file_names: Vec<String>,
    /// Force IPv4 (`--inet4-only` / `-4`).
    pub inet4_only: bool,
    /// Force IPv6 (`--inet6-only` / `-6`).
    pub inet6_only: bool,
    /// Address family preference (`--prefer-family`).
    pub prefer_family: String,
    /// Retry even after connection refused (`--retry-connrefused`).
    pub retry_connrefused: bool,
    /// Generic username (`--user`).
    pub user: Option<String>,
    /// Generic password (`--password`).
    pub password: Option<String>,
    /// Prompt for a password (`--ask-password`).
    pub ask_password: bool,
    /// External askpass program (`--use-askpass`).
    pub use_askpass: Option<String>,
    /// Allow IRIs / non-ASCII URLs (default true; `--no-iri` clears).
    pub iri: bool,
    /// Local character encoding (`--local-encoding`).
    pub local_encoding: Option<String>,
    /// Remote character encoding (`--remote-encoding`).
    pub remote_encoding: Option<String>,
    /// Unlink file before clobbering (`--unlink`).
    pub unlink: bool,
    /// Store metadata in extended attributes (`--xattr`).
    pub xattr: bool,
    /// Keep Metalink downloads with checksum mismatch (`--keep-badhash`).
    pub keep_badhash: bool,
    /// Use Metalink metadata from HTTP headers (`--metalink-over-http`).
    pub metalink_over_http: bool,
    /// Metalink metaurl ordinal (`--metalink-index`).
    pub metalink_index: i32,
    /// Preferred Metalink location (`--preferred-location`).
    pub preferred_location: Option<String>,
    /// Maximum concurrent downloads (`--max-threads`). Default 1 = serial.
    pub max_threads: u32,
    /// Max concurrent downloads per host (`--max-threads-per-host`).
    /// `0` means unset: resolve to `min(max_threads, 4)` via
    /// [`Config::effective_max_threads_per_host`].
    pub max_threads_per_host: u32,

    /// Create directories as needed (default true; `--no-directories` clears).
    pub directories: bool,
    /// Force directory creation (`--force-directories` / `-x`).
    pub force_directories: bool,
    /// Create host-named directories (default true).
    pub host_directories: bool,
    /// Create protocol-named directories (`--protocol-directories`).
    pub protocol_directories: bool,
    /// Ignore N remote directory components (`--cut-dirs`).
    pub cut_dirs: u32,
    /// Local directory prefix (`--directory-prefix` / `-P`).
    pub directory_prefix: String,

    /// Default local name for directory URLs (`--default-page`).
    pub default_page: String,
    /// Append type-based extensions (`--adjust-extension` / `-E`).
    pub adjust_extension: bool,
    /// HTTP username (`--http-user`).
    pub http_user: Option<String>,
    /// HTTP password (`--http-password`).
    pub http_password: Option<String>,
    /// HTTP keep-alive (default true).
    pub http_keep_alive: bool,
    /// Allow cache / conditional requests (default true).
    pub cache: bool,
    /// Use cookies (default true; `--no-cookies` clears).
    pub cookies: bool,
    /// Load cookies from file (`--load-cookies`).
    pub load_cookies: Option<String>,
    /// Save cookies to file (`--save-cookies`).
    pub save_cookies: Option<String>,
    /// Keep session cookies when saving (`--keep-session-cookies`).
    pub keep_session_cookies: bool,
    /// Ignore `Content-Length` (`--ignore-length`).
    pub ignore_length: bool,
    /// Extra HTTP headers (`--header`).
    pub headers: Vec<String>,
    /// Content-Encoding handling (`--compression`).
    pub compression: String,
    /// Maximum redirects to follow (`--max-redirect`).
    pub max_redirect: u32,
    /// Proxy username (`--proxy-user`).
    pub proxy_user: Option<String>,
    /// Proxy password (`--proxy-password`).
    pub proxy_password: Option<String>,
    /// Referer header (`--referer`).
    pub referer: Option<String>,
    /// Save server headers with the document (`--save-headers`).
    pub save_headers: bool,
    /// User-Agent string (`--user-agent` / `-U`).
    pub user_agent: String,
    /// POST body data (`--post-data`).
    pub post_data: Option<String>,
    /// POST body from file (`--post-file`).
    pub post_file: Option<String>,
    /// Custom HTTP method (`--method`).
    pub method: Option<String>,
    /// Request body data (`--body-data`).
    pub body_data: Option<String>,
    /// Request body from file (`--body-file`).
    pub body_file: Option<String>,
    /// Honor `Content-Disposition` filename (`--content-disposition`).
    pub content_disposition: bool,
    /// Save error-page bodies (`--content-on-error`).
    pub content_on_error: bool,
    /// Use last redirect component as local name (`--trust-server-names`).
    pub trust_server_names: bool,
    /// Send Basic auth without waiting for a challenge (`--auth-no-challenge`).
    pub auth_no_challenge: bool,
    /// Retry after host/DNS errors (`--retry-on-host-error`).
    pub retry_on_host_error: bool,
    /// HTTP status codes that should be retried (`--retry-on-http-error`).
    pub retry_on_http_error: Vec<u16>,
    /// Read `.netrc` for credentials (default true).
    pub netrc: bool,
    /// Alternate `.netrc` path (`--netrc-file`).
    pub netrc_file: Option<String>,

    /// TLS protocol selection (`--secure-protocol`).
    pub secure_protocol: String,
    /// Refuse non-HTTPS URLs (`--https-only`).
    pub https_only: bool,
    /// Verify server certificates (default true).
    pub check_certificate: bool,
    /// Client certificate file (`--certificate`).
    pub certificate: Option<String>,
    /// Client certificate type (`--certificate-type`).
    pub certificate_type: String,
    /// Private key file (`--private-key`).
    pub private_key: Option<String>,
    /// Private key type (`--private-key-type`).
    pub private_key_type: String,
    /// CA bundle file (`--ca-certificate`).
    pub ca_certificate: Option<String>,
    /// CA directory (`--ca-directory`).
    pub ca_directory: Option<String>,
    /// CRL file (`--crl-file`).
    pub crl_file: Option<String>,
    /// Pin expected public key (`--pinnedpubkey`).
    pub pinnedpubkey: Option<String>,
    /// OpenSSL-style random file (accepted for wget compatibility).
    pub random_file: Option<String>,
    /// OpenSSL-style EGD socket (accepted for wget compatibility).
    pub egd_file: Option<String>,
    /// Honor HSTS (default true).
    pub hsts: bool,
    /// HSTS database path (`--hsts-file`).
    pub hsts_file: Option<String>,

    /// WARC output file prefix (`--warc-file`).
    pub warc_file: Option<String>,
    /// Extra WARC headers (`--warc-header`).
    pub warc_header: Vec<String>,
    /// Max WARC file size before rotation (`--warc-max-size`).
    pub warc_max_size: Option<u64>,
    /// Write CDX index (`--warc-cdx`).
    pub warc_cdx: bool,
    /// WARC deduplication CDX (`--warc-dedup`).
    pub warc_dedup: Option<String>,
    /// Compress WARC records (default true).
    pub warc_compression: bool,
    /// Record WARC digests (default true).
    pub warc_digests: bool,
    /// Keep WARC log (default true).
    pub warc_keep_log: bool,
    /// WARC temporary directory (`--warc-tempdir`).
    pub warc_tempdir: Option<String>,

    /// FTP username (`--ftp-user`).
    pub ftp_user: Option<String>,
    /// FTP password (`--ftp-password`).
    pub ftp_password: Option<String>,
    /// Remove `.listing` files after FTP (default true).
    pub remove_listing: bool,
    /// Expand FTP globs (default true).
    pub ftp_glob: bool,
    /// Use passive FTP (default true).
    pub passive_ftp: bool,
    /// Preserve remote permissions (`--preserve-permissions`).
    pub preserve_permissions: bool,
    /// Retrieve through symlinks (`--retr-symlinks`).
    pub retr_symlinks: bool,
    /// Implicit FTPS (`--ftps-implicit`).
    pub ftps_implicit: bool,
    /// Resume TLS on FTPS data connection (default true).
    pub ftps_resume_ssl: bool,
    /// Clear FTPS data connection (`--ftps-clear-data-connection`).
    pub ftps_clear_data_connection: bool,
    /// Fall back from FTPS to FTP (`--ftps-fallback-to-ftp`).
    pub ftps_fallback_to_ftp: bool,

    /// Recursive retrieval (`--recursive` / `-r`).
    pub recursive: bool,
    /// Maximum recursion depth; `-1` means infinite (`--level` / `-l`).
    pub level: i32,
    /// Delete files after download (`--delete-after`).
    pub delete_after: bool,
    /// Convert links for local viewing (`--convert-links` / `-k`).
    pub convert_links: bool,
    /// Convert only the file portion of links (`--convert-file-only`).
    pub convert_file_only: bool,
    /// Back up converted files (`--backup-converted` / `-K`).
    pub backup_converted: bool,
    /// Mirror shorthand (`--mirror` / `-m`); see [`Config::apply_mirror`].
    pub mirror: bool,
    /// Download page requisites (`--page-requisites` / `-p`).
    pub page_requisites: bool,
    /// Strict HTML comment parsing (`--strict-comments`).
    pub strict_comments: bool,

    /// Accept name globs (`--accept` / `-A`).
    pub accept: Vec<String>,
    /// Reject name globs (`--reject` / `-R`).
    pub reject: Vec<String>,
    /// Accept MIME types (`--accept` MIME / `--filter-mime-type` family).
    pub filter_mime_type: Vec<String>,
    /// Query keys to strip from URLs (`--cut-url-get-vars`); `None` = unchanged, `Some([])` = strip all.
    pub cut_url_get_vars: Option<Vec<String>>,
    /// Query keys to strip from local filenames (`--cut-file-get-vars`).
    pub cut_file_get_vars: Option<Vec<String>>,
    /// Accept URL regex (`--accept-regex`).
    pub accept_regex: Option<String>,
    /// Reject URL regex (`--reject-regex`).
    pub reject_regex: Option<String>,
    /// Regex engine type (`--regex-type`).
    pub regex_type: String,
    /// Allowed domains (`--domains` / `-D`).
    pub domains: Vec<String>,
    /// Excluded domains (`--exclude-domains`).
    pub exclude_domains: Vec<String>,
    /// Follow FTP links from HTML (`--follow-ftp`).
    pub follow_ftp: bool,
    /// HTML tags to follow (`--follow-tags`).
    pub follow_tags: Vec<String>,
    /// HTML tags to ignore (`--ignore-tags`).
    pub ignore_tags: Vec<String>,
    /// Case-insensitive matching (`--ignore-case`).
    pub ignore_case: bool,
    /// Span hosts when recursing (`--span-hosts` / `-H`).
    pub span_hosts: bool,
    /// Follow only relative links (`--relative` / `-L`).
    pub relative_only: bool,
    /// Include directory prefixes (`--include-directories` / `-I`).
    pub include_directories: Vec<String>,
    /// Exclude directory prefixes (`--exclude-directories` / `-X`).
    pub exclude_directories: Vec<String>,
    /// Do not ascend to the parent directory (`--no-parent` / `-np`).
    pub no_parent: bool,

    /// Positional URL arguments from the CLI.
    pub urls: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            background: false,
            logfile: None,
            append_output: false,
            debug: false,
            quiet: false,
            verbose: true,
            report_speed_bits: false,
            input_file: None,
            input_metalink: None,
            force_html: false,
            force_css: false,
            force_metalink: false,
            force_rss: false,
            force_atom: false,
            force_sitemap: false,
            follow_sitemaps: false,
            base: None,
            config_file: None,
            no_config: false,
            rejected_log: None,
            execute_commands: Vec::new(),

            bind_address: None,
            bind_dns_address: None,
            dns_servers: None,
            tries: 20,
            output_document: None,
            no_clobber: false,
            unique_names: false,
            backups: 0,
            continue_download: false,
            start_pos: None,
            progress: "bar".into(),
            show_progress: false,
            timestamping: false,
            if_modified_since: true,
            use_server_timestamps: true,
            server_response: false,
            spider: false,
            timeout: None,
            dns_timeout: None,
            connect_timeout: None,
            read_timeout: Some(900.0),
            limit_rate: None,
            wait: 0.0,
            waitretry: 10.0,
            random_wait: false,
            use_proxy: true,
            http_proxy: None,
            https_proxy: None,
            quota: None,
            dns_cache: true,
            restrict_file_names: vec!["unix".into()],
            inet4_only: false,
            inet6_only: false,
            prefer_family: "none".into(),
            retry_connrefused: false,
            user: None,
            password: None,
            ask_password: false,
            use_askpass: None,
            iri: true,
            local_encoding: None,
            remote_encoding: None,
            unlink: false,
            xattr: false,
            keep_badhash: false,
            metalink_over_http: false,
            metalink_index: 0,
            preferred_location: None,
            max_threads: 1,
            max_threads_per_host: 0,

            directories: true,
            force_directories: false,
            host_directories: true,
            protocol_directories: false,
            cut_dirs: 0,
            directory_prefix: ".".into(),

            default_page: "index.html".into(),
            adjust_extension: false,
            http_user: None,
            http_password: None,
            http_keep_alive: true,
            cache: true,
            cookies: true,
            load_cookies: None,
            save_cookies: None,
            keep_session_cookies: false,
            ignore_length: false,
            headers: Vec::new(),
            compression: "none".into(),
            max_redirect: 20,
            proxy_user: None,
            proxy_password: None,
            referer: None,
            save_headers: false,
            user_agent: format!("fetchling/{}", env!("CARGO_PKG_VERSION")),
            post_data: None,
            post_file: None,
            method: None,
            body_data: None,
            body_file: None,
            content_disposition: false,
            content_on_error: false,
            trust_server_names: false,
            auth_no_challenge: false,
            retry_on_host_error: false,
            retry_on_http_error: Vec::new(),
            netrc: true,
            netrc_file: None,

            secure_protocol: "auto".into(),
            https_only: false,
            check_certificate: true,
            certificate: None,
            certificate_type: "pem".into(),
            private_key: None,
            private_key_type: "pem".into(),
            ca_certificate: None,
            ca_directory: None,
            crl_file: None,
            pinnedpubkey: None,
            random_file: None,
            egd_file: None,
            hsts: true,
            hsts_file: None,

            warc_file: None,
            warc_header: Vec::new(),
            warc_max_size: None,
            warc_cdx: false,
            warc_dedup: None,
            warc_compression: true,
            warc_digests: true,
            warc_keep_log: true,
            warc_tempdir: None,

            ftp_user: None,
            ftp_password: None,
            remove_listing: true,
            ftp_glob: true,
            passive_ftp: true,
            preserve_permissions: false,
            retr_symlinks: false,
            ftps_implicit: false,
            ftps_resume_ssl: true,
            ftps_clear_data_connection: false,
            ftps_fallback_to_ftp: false,

            recursive: false,
            level: 5,
            delete_after: false,
            convert_links: false,
            convert_file_only: false,
            backup_converted: false,
            mirror: false,
            page_requisites: false,
            strict_comments: false,

            accept: Vec::new(),
            reject: Vec::new(),
            filter_mime_type: Vec::new(),
            cut_url_get_vars: None,
            cut_file_get_vars: None,
            accept_regex: None,
            reject_regex: None,
            regex_type: "posix".into(),
            domains: Vec::new(),
            exclude_domains: Vec::new(),
            follow_ftp: false,
            follow_tags: Vec::new(),
            ignore_tags: Vec::new(),
            ignore_case: false,
            span_hosts: false,
            relative_only: false,
            include_directories: Vec::new(),
            exclude_directories: Vec::new(),
            no_parent: false,

            urls: Vec::new(),
        }
    }
}

impl Config {
    /// Apply mirror shorthand (`-m`): recursive, infinite level, timestamping, keep listing.
    pub fn apply_mirror(&mut self) {
        self.recursive = true;
        self.level = -1; // infinite
        self.timestamping = true;
        self.remove_listing = false;
    }

    /// Apply `-T` / `--timeout`, setting DNS, connect, and read timeouts together.
    pub fn apply_timeout(&mut self, seconds: f64) {
        self.timeout = Some(seconds);
        self.dns_timeout = Some(seconds);
        self.connect_timeout = Some(seconds);
        self.read_timeout = Some(seconds);
    }

    /// Per-host concurrency: explicit `--max-threads-per-host`, or `min(max_threads, 4)`.
    pub fn effective_max_threads_per_host(&self) -> u32 {
        if self.max_threads_per_host == 0 {
            self.max_threads.clamp(1, 4)
        } else {
            self.max_threads_per_host
        }
    }

    /// Resolve unset `max_threads_per_host` (`0`) to [`Self::effective_max_threads_per_host`].
    pub fn finalize_concurrency(&mut self) {
        if self.max_threads_per_host == 0 {
            self.max_threads_per_host = self.effective_max_threads_per_host();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_mirror_sets_recursive_infinite_timestamping() {
        let mut c = Config {
            remove_listing: true,
            ..Default::default()
        };
        c.apply_mirror();
        assert!(c.recursive);
        assert_eq!(c.level, -1);
        assert!(c.timestamping);
        assert!(!c.remove_listing);
    }

    #[test]
    fn apply_timeout_sets_all_timeout_fields() {
        let mut c = Config::default();
        c.apply_timeout(30.0);
        assert_eq!(c.timeout, Some(30.0));
        assert_eq!(c.dns_timeout, Some(30.0));
        assert_eq!(c.connect_timeout, Some(30.0));
        assert_eq!(c.read_timeout, Some(30.0));
    }

    #[test]
    fn effective_max_threads_per_host_defaults_to_min_of_max_and_four() {
        let mut c = Config {
            max_threads: 8,
            max_threads_per_host: 0,
            ..Default::default()
        };
        assert_eq!(c.effective_max_threads_per_host(), 4);

        c.max_threads = 2;
        assert_eq!(c.effective_max_threads_per_host(), 2);

        c.max_threads_per_host = 3;
        assert_eq!(c.effective_max_threads_per_host(), 3);
    }

    #[test]
    fn finalize_concurrency_fills_unset_host_limit() {
        let mut c = Config {
            max_threads: 8,
            max_threads_per_host: 0,
            ..Default::default()
        };
        c.finalize_concurrency();
        assert_eq!(c.max_threads_per_host, 4);

        let mut c = Config {
            max_threads: 8,
            max_threads_per_host: 2,
            ..Default::default()
        };
        c.finalize_concurrency();
        assert_eq!(c.max_threads_per_host, 2);
    }
}
