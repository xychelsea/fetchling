use fetchling_core::{
    apply_wgetrc_command, load_wgetrc_files, parse_bytes, parse_seconds, parse_tries, Config,
    Error, Result,
};

use crate::deferred::is_deferred_option;
use crate::options::OPTIONS;

#[derive(Debug)]
pub enum ParseOutcome {
    Help,
    /// Long `--version` output (name, description, license).
    Version,
    /// Short `-V` output (name and version only).
    VersionShort,
    Run(Box<Config>),
}

pub fn parse_args<I, S>(args: I) -> Result<ParseOutcome>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cfg = Config::default();
    let mut args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    // Skip argv[0] when callers pass full argv (tests, wrappers). `main` already
    // skips it; this still drops paths like `./fetchling` that an older heuristic
    // misclassified as URLs because they contain '.'.
    if args.first().is_some_and(|a| looks_like_program_name(a)) {
        args.remove(0);
    }

    prescanner_config_flags(&args, &mut cfg)?;
    load_wgetrc_files(&mut cfg)?;

    let mut i = 0;
    let mut end_of_opts = false;
    while i < args.len() {
        let arg = args[i].clone();
        if end_of_opts || arg == "--" {
            if arg == "--" {
                end_of_opts = true;
                i += 1;
                continue;
            }
            cfg.urls.push(arg);
            i += 1;
            continue;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };

            if is_deferred_option(name) {
                return Err(Error::DeferredOption(name.to_string()));
            }

            if name == "help" {
                return Ok(ParseOutcome::Help);
            }
            if name == "version" {
                return Ok(ParseOutcome::Version);
            }

            let meta = OPTIONS.iter().find(|o| o.long == name);
            if meta.is_none() && !name.starts_with("no-") {
                if let Some(rest) = name.strip_prefix("no-") {
                    if OPTIONS.iter().any(|o| o.long == rest) {
                        apply_long(&mut cfg, name, None)?;
                        i += 1;
                        continue;
                    }
                }
                return Err(Error::InvalidOption(format!(
                    "unrecognized option '--{name}'"
                )));
            }

            let needs_value = meta.map(|m| m.takes_value).unwrap_or(false);
            let value = if needs_value {
                if let Some(v) = inline {
                    Some(v)
                } else {
                    i += 1;
                    if i >= args.len() {
                        return Err(Error::InvalidOption(format!(
                            "option '--{name}' requires an argument"
                        )));
                    }
                    Some(args[i].clone())
                }
            } else {
                inline
            };
            apply_long(&mut cfg, name, value.as_deref())?;
            i += 1;
            continue;
        }

        if arg.starts_with('-') && arg != "-" {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut cidx = 0;
            while cidx < chars.len() {
                let ch = chars[cidx];
                match ch {
                    'h' => return Ok(ParseOutcome::Help),
                    'V' => return Ok(ParseOutcome::VersionShort),
                    'n' => {
                        // -nX packs --no-* shorts (nc, nd, nH, np, nv)
                        cidx += 1;
                        while cidx < chars.len() {
                            match chars[cidx] {
                                'v' => apply_long(&mut cfg, "no-verbose", None)?,
                                'c' => apply_long(&mut cfg, "no-clobber", None)?,
                                'd' => apply_long(&mut cfg, "no-directories", None)?,
                                'H' => apply_long(&mut cfg, "no-host-directories", None)?,
                                'p' => apply_long(&mut cfg, "no-parent", None)?,
                                other => {
                                    return Err(Error::InvalidOption(format!(
                                        "invalid option -- 'n{other}'"
                                    )));
                                }
                            }
                            cidx += 1;
                        }
                        break;
                    }
                    _ => {}
                }
                let meta = OPTIONS.iter().find(|o| o.short == Some(ch));
                let Some(meta) = meta else {
                    return Err(Error::InvalidOption(format!("invalid option -- '{ch}'")));
                };
                if meta.takes_value {
                    let value = if cidx + 1 < chars.len() {
                        // -Ofile
                        let rest: String = chars[cidx + 1..].iter().collect();
                        apply_long(&mut cfg, meta.long, Some(&rest))?;
                        break;
                    } else {
                        i += 1;
                        if i >= args.len() {
                            return Err(Error::InvalidOption(format!(
                                "option requires an argument -- '{ch}'"
                            )));
                        }
                        args[i].clone()
                    };
                    apply_long(&mut cfg, meta.long, Some(&value))?;
                } else {
                    apply_long(&mut cfg, meta.long, None)?;
                    cidx += 1;
                    continue;
                }
                break;
            }
            i += 1;
            continue;
        }

        cfg.urls.push(arg);
        i += 1;
    }

    // Apply pending -e commands after options (after wgetrc file load).
    let cmds = cfg.execute_commands.clone();
    for cmd in cmds {
        apply_wgetrc_command(&mut cfg, &cmd)?;
    }

    Ok(ParseOutcome::Run(Box::new(cfg)))
}

fn apply_long(cfg: &mut Config, name: &str, value: Option<&str>) -> Result<()> {
    let need = |v: Option<&str>, n: &str| -> Result<String> {
        v.map(|s| s.to_string())
            .ok_or_else(|| Error::InvalidOption(format!("option '--{n}' requires an argument")))
    };

    match name {
        "background" => cfg.background = true,
        "execute" => cfg.execute_commands.push(need(value, name)?),
        "output-file" => {
            cfg.logfile = Some(need(value, name)?);
            cfg.append_output = false;
        }
        "append-output" => {
            cfg.logfile = Some(need(value, name)?);
            cfg.append_output = true;
        }
        "debug" => cfg.debug = true,
        "quiet" => {
            cfg.quiet = true;
            cfg.verbose = false;
        }
        "verbose" => {
            cfg.verbose = true;
            cfg.quiet = false;
        }
        "no-verbose" => cfg.verbose = false,
        "report-speed" => {
            cfg.report_speed_bits = need(value, name)?.eq_ignore_ascii_case("bits");
        }
        "input-file" => cfg.input_file = Some(need(value, name)?),
        "input-metalink" => cfg.input_metalink = Some(need(value, name)?),
        "keep-badhash" => cfg.keep_badhash = true,
        "metalink-over-http" => cfg.metalink_over_http = true,
        "metalink-index" => {
            let v = need(value, name)?;
            cfg.metalink_index = if v.eq_ignore_ascii_case("inf") {
                0
            } else {
                v.parse()
                    .map_err(|_| Error::Parse(format!("bad metalink-index: {v}")))?
            };
        }
        "preferred-location" => cfg.preferred_location = Some(need(value, name)?),
        "force-html" => cfg.force_html = true,
        "force-css" => cfg.force_css = true,
        "force-metalink" => cfg.force_metalink = true,
        "force-rss" => cfg.force_rss = true,
        "force-atom" => cfg.force_atom = true,
        "force-sitemap" => cfg.force_sitemap = true,
        "base" => cfg.base = Some(need(value, name)?),
        "config" => cfg.config_file = Some(need(value, name)?),
        "no-config" => cfg.no_config = true,
        "rejected-log" => cfg.rejected_log = Some(need(value, name)?),
        "bind-address" => cfg.bind_address = Some(need(value, name)?),
        "bind-dns-address" => cfg.bind_dns_address = Some(need(value, name)?),
        "dns-servers" => cfg.dns_servers = Some(need(value, name)?),
        "tries" => cfg.tries = parse_tries(&need(value, name)?)?,
        "output-document" => cfg.output_document = Some(need(value, name)?),
        "no-clobber" => {
            cfg.no_clobber = true;
            cfg.unique_names = false;
        }
        "clobber" => {
            cfg.no_clobber = false;
            cfg.unique_names = false;
        }
        "unique-names" => {
            cfg.unique_names = true;
            cfg.no_clobber = false;
        }
        "backups" => {
            cfg.backups = need(value, name)?
                .parse()
                .map_err(|_| Error::Parse("bad backups".into()))?
        }
        "continue" => cfg.continue_download = true,
        "start-pos" => cfg.start_pos = Some(parse_bytes(&need(value, name)?)?.get()),
        "progress" => cfg.progress = need(value, name)?,
        "show-progress" | "force-progress" => cfg.show_progress = true,
        "timestamping" => cfg.timestamping = true,
        "no-timestamping" => cfg.timestamping = false,
        "no-if-modified-since" => cfg.if_modified_since = false,
        "if-modified-since" => cfg.if_modified_since = true,
        "no-use-server-timestamps" => cfg.use_server_timestamps = false,
        "use-server-timestamps" => cfg.use_server_timestamps = true,
        "server-response" => cfg.server_response = true,
        "spider" => cfg.spider = true,
        "timeout" => cfg.apply_timeout(parse_seconds(&need(value, name)?)?),
        "dns-timeout" => cfg.dns_timeout = Some(parse_seconds(&need(value, name)?)?),
        "connect-timeout" => cfg.connect_timeout = Some(parse_seconds(&need(value, name)?)?),
        "read-timeout" => cfg.read_timeout = Some(parse_seconds(&need(value, name)?)?),
        "limit-rate" => cfg.limit_rate = Some(parse_bytes(&need(value, name)?)?.get()),
        "wait" => cfg.wait = parse_seconds(&need(value, name)?)?,
        "waitretry" => cfg.waitretry = parse_seconds(&need(value, name)?)?,
        "random-wait" => cfg.random_wait = true,
        "no-proxy" => cfg.use_proxy = false,
        "http-proxy" => cfg.http_proxy = Some(need(value, name)?),
        "https-proxy" => cfg.https_proxy = Some(need(value, name)?),
        "proxy" => {
            let v = need(value, name)?;
            if cfg.http_proxy.is_none() {
                cfg.http_proxy = Some(v.clone());
            }
            if cfg.https_proxy.is_none() {
                cfg.https_proxy = Some(v);
            }
        }
        "quota" => cfg.quota = Some(parse_bytes(&need(value, name)?)?.get()),
        "no-dns-cache" => cfg.dns_cache = false,
        "dns-cache" => cfg.dns_cache = true,
        "restrict-file-names" => {
            cfg.restrict_file_names = split_list(&need(value, name)?);
        }
        "inet4-only" => {
            cfg.inet4_only = true;
            cfg.inet6_only = false;
        }
        "inet6-only" => {
            cfg.inet6_only = true;
            cfg.inet4_only = false;
        }
        "prefer-family" => cfg.prefer_family = need(value, name)?,
        "retry-connrefused" => cfg.retry_connrefused = true,
        "user" => cfg.user = Some(need(value, name)?),
        "password" => cfg.password = Some(need(value, name)?),
        "ask-password" => cfg.ask_password = true,
        "use-askpass" => cfg.use_askpass = Some(need(value, name)?),
        "no-iri" => cfg.iri = false,
        "iri" => cfg.iri = true,
        "local-encoding" => cfg.local_encoding = Some(need(value, name)?),
        "remote-encoding" => cfg.remote_encoding = Some(need(value, name)?),
        "unlink" => cfg.unlink = true,
        "xattr" => cfg.xattr = true,
        "no-directories" => cfg.directories = false,
        "directories" => cfg.directories = true,
        "force-directories" => {
            cfg.force_directories = true;
            cfg.directories = true;
        }
        "no-host-directories" => cfg.host_directories = false,
        "host-directories" => cfg.host_directories = true,
        "protocol-directories" => cfg.protocol_directories = true,
        "cut-dirs" => {
            cfg.cut_dirs = need(value, name)?
                .parse()
                .map_err(|_| Error::Parse("bad cut-dirs".into()))?
        }
        "directory-prefix" => cfg.directory_prefix = need(value, name)?,
        "default-page" => cfg.default_page = need(value, name)?,
        "adjust-extension" | "html-extension" => cfg.adjust_extension = true,
        "http-user" => cfg.http_user = Some(need(value, name)?),
        "http-password" => cfg.http_password = Some(need(value, name)?),
        "no-http-keep-alive" => cfg.http_keep_alive = false,
        "http-keep-alive" => cfg.http_keep_alive = true,
        "max-threads" => {
            let n: u32 = need(value, name)?
                .parse()
                .map_err(|_| Error::Parse("bad max-threads".into()))?;
            if !(1..=32).contains(&n) {
                return Err(Error::Parse("max-threads must be 1..=32".into()));
            }
            cfg.max_threads = n;
        }
        "no-cache" => cfg.cache = false,
        "cache" => cfg.cache = true,
        "no-cookies" => cfg.cookies = false,
        "cookies" => cfg.cookies = true,
        "load-cookies" => cfg.load_cookies = Some(need(value, name)?),
        "save-cookies" => cfg.save_cookies = Some(need(value, name)?),
        "keep-session-cookies" => cfg.keep_session_cookies = true,
        "ignore-length" => cfg.ignore_length = true,
        "header" => cfg.headers.push(need(value, name)?),
        "compression" => cfg.compression = need(value, name)?,
        "max-redirect" => {
            cfg.max_redirect = need(value, name)?
                .parse()
                .map_err(|_| Error::Parse("bad max-redirect".into()))?
        }
        "proxy-user" => cfg.proxy_user = Some(need(value, name)?),
        "proxy-password" => cfg.proxy_password = Some(need(value, name)?),
        "referer" => cfg.referer = Some(need(value, name)?),
        "save-headers" => cfg.save_headers = true,
        "user-agent" => cfg.user_agent = need(value, name)?,
        "post-data" => cfg.post_data = Some(need(value, name)?),
        "post-file" => cfg.post_file = Some(need(value, name)?),
        "method" => cfg.method = Some(need(value, name)?.to_ascii_uppercase()),
        "body-data" => cfg.body_data = Some(need(value, name)?),
        "body-file" => cfg.body_file = Some(need(value, name)?),
        "content-disposition" => cfg.content_disposition = true,
        "content-on-error" => cfg.content_on_error = true,
        "trust-server-names" => cfg.trust_server_names = true,
        "auth-no-challenge" => cfg.auth_no_challenge = true,
        "retry-on-host-error" => cfg.retry_on_host_error = true,
        "retry-on-http-error" => {
            cfg.retry_on_http_error = need(value, name)?
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse()
                        .map_err(|_| Error::Parse(format!("bad http code: {s}")))
                })
                .collect::<Result<Vec<_>>>()?;
        }
        "no-netrc" => cfg.netrc = false,
        "netrc" => cfg.netrc = true,
        "netrc-file" => cfg.netrc_file = Some(need(value, name)?),
        "secure-protocol" => cfg.secure_protocol = need(value, name)?,
        "https-only" => cfg.https_only = true,
        "ciphers" => cfg.ciphers = Some(need(value, name)?),
        "no-check-certificate" => cfg.check_certificate = false,
        "check-certificate" => cfg.check_certificate = true,
        "certificate" => cfg.certificate = Some(need(value, name)?),
        "certificate-type" => cfg.certificate_type = need(value, name)?,
        "private-key" => cfg.private_key = Some(need(value, name)?),
        "private-key-type" => cfg.private_key_type = need(value, name)?,
        "ca-certificate" => cfg.ca_certificate = Some(need(value, name)?),
        "ca-directory" => cfg.ca_directory = Some(need(value, name)?),
        "crl-file" => cfg.crl_file = Some(need(value, name)?),
        "pinnedpubkey" => cfg.pinnedpubkey = Some(need(value, name)?),
        "random-file" => cfg.random_file = Some(need(value, name)?),
        "egd-file" => cfg.egd_file = Some(need(value, name)?),
        "no-hsts" => cfg.hsts = false,
        "hsts" => cfg.hsts = true,
        "hsts-file" => cfg.hsts_file = Some(need(value, name)?),
        "warc-file" => cfg.warc_file = Some(need(value, name)?),
        "warc-header" => cfg.warc_header.push(need(value, name)?),
        "warc-max-size" => cfg.warc_max_size = Some(parse_bytes(&need(value, name)?)?.get()),
        "warc-cdx" => cfg.warc_cdx = true,
        "warc-dedup" => cfg.warc_dedup = Some(need(value, name)?),
        "no-warc-compression" => cfg.warc_compression = false,
        "warc-compression" => cfg.warc_compression = true,
        "no-warc-digests" => cfg.warc_digests = false,
        "warc-digests" => cfg.warc_digests = true,
        "no-warc-keep-log" => cfg.warc_keep_log = false,
        "warc-keep-log" => cfg.warc_keep_log = true,
        "warc-tempdir" => cfg.warc_tempdir = Some(need(value, name)?),
        "ftp-user" => cfg.ftp_user = Some(need(value, name)?),
        "ftp-password" => cfg.ftp_password = Some(need(value, name)?),
        "no-remove-listing" => cfg.remove_listing = false,
        "remove-listing" => cfg.remove_listing = true,
        "no-glob" => cfg.ftp_glob = false,
        "glob" => cfg.ftp_glob = true,
        "no-passive-ftp" => cfg.passive_ftp = false,
        "passive-ftp" => cfg.passive_ftp = true,
        "preserve-permissions" => cfg.preserve_permissions = true,
        "retr-symlinks" => cfg.retr_symlinks = true,
        "ftps-implicit" => cfg.ftps_implicit = true,
        "no-ftps-resume-ssl" => cfg.ftps_resume_ssl = false,
        "ftps-resume-ssl" => cfg.ftps_resume_ssl = true,
        "ftps-clear-data-connection" => cfg.ftps_clear_data_connection = true,
        "ftps-fallback-to-ftp" => cfg.ftps_fallback_to_ftp = true,
        "recursive" => cfg.recursive = true,
        "level" => {
            let v = need(value, name)?;
            cfg.level = if v.eq_ignore_ascii_case("inf") || v == "0" {
                -1
            } else {
                v.parse()
                    .map_err(|_| Error::Parse(format!("bad level: {v}")))?
            };
        }
        "delete-after" => cfg.delete_after = true,
        "convert-links" => cfg.convert_links = true,
        "convert-file-only" => cfg.convert_file_only = true,
        "backup-converted" => cfg.backup_converted = true,
        "mirror" => cfg.apply_mirror(),
        "page-requisites" => cfg.page_requisites = true,
        "strict-comments" => cfg.strict_comments = true,
        "accept" => cfg.accept = split_list(&need(value, name)?),
        "reject" => cfg.reject = split_list(&need(value, name)?),
        "filter-mime-type" => {
            cfg.filter_mime_type.extend(split_list(&need(value, name)?));
        }
        "cut-url-get-vars" => {
            let v = need(value, name)?;
            cfg.cut_url_get_vars = Some(split_list(&v));
        }
        "cut-file-get-vars" => {
            let v = need(value, name)?;
            cfg.cut_file_get_vars = Some(split_list(&v));
        }
        "accept-regex" => cfg.accept_regex = Some(need(value, name)?),
        "reject-regex" => cfg.reject_regex = Some(need(value, name)?),
        "regex-type" => cfg.regex_type = need(value, name)?,
        "domains" => cfg.domains = split_list(&need(value, name)?),
        "exclude-domains" => cfg.exclude_domains = split_list(&need(value, name)?),
        "follow-ftp" => cfg.follow_ftp = true,
        "follow-sitemaps" => cfg.follow_sitemaps = true,
        "follow-tags" => cfg.follow_tags = split_list(&need(value, name)?),
        "ignore-tags" => cfg.ignore_tags = split_list(&need(value, name)?),
        "ignore-case" => cfg.ignore_case = true,
        "span-hosts" => cfg.span_hosts = true,
        "relative" => cfg.relative_only = true,
        "include-directories" => cfg.include_directories = split_list(&need(value, name)?),
        "exclude-directories" => cfg.exclude_directories = split_list(&need(value, name)?),
        "no-parent" => cfg.no_parent = true,
        "parent" => cfg.no_parent = false,
        "help" | "version" => {}
        other => {
            if is_deferred_option(other) {
                return Err(Error::DeferredOption(other.to_string()));
            }
            return Err(Error::InvalidOption(format!(
                "unrecognized option '--{other}'"
            )));
        }
    }
    Ok(())
}

fn split_list(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Scan argv for `--no-config` / `--config` before loading wgetrc files.
fn prescanner_config_flags(args: &[String], cfg: &mut Config) -> Result<()> {
    let mut i = 0;
    let mut end_of_opts = false;
    while i < args.len() {
        let arg = &args[i];
        if end_of_opts || arg == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }
        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (body, None),
            };
            if name == "no-config" {
                cfg.no_config = true;
                cfg.config_file = None;
            } else if name == "config" {
                let value = if let Some(v) = inline {
                    v.to_string()
                } else {
                    i += 1;
                    if i >= args.len() {
                        return Err(Error::InvalidOption(
                            "option '--config' requires an argument".into(),
                        ));
                    }
                    args[i].clone()
                };
                cfg.config_file = Some(value);
                cfg.no_config = false;
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// True when `arg` is the binary name / path (`fetchling`, `./fetchling`, …),
/// not a download URL. Basename match avoids treating `example.com/path` as argv0.
fn looks_like_program_name(arg: &str) -> bool {
    if arg.starts_with('-') || arg.contains("://") {
        return false;
    }
    let base = arg.rsplit('/').next().unwrap_or(arg);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base);
    base == "fetchling"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let out = parse_args(["fetchling", "-q", "-O", "-", "http://example.com"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(c.quiet);
                assert_eq!(c.output_document.as_deref(), Some("-"));
                assert_eq!(c.urls, vec!["http://example.com"]);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn strips_dot_slash_program_path() {
        let out = parse_args(["./fetchling", "http://example.com/file.bin"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert_eq!(c.urls, vec!["http://example.com/file.bin"]);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn strips_absolute_program_path() {
        let out = parse_args([
            "/projects/xychelsea/fetchling/target/debug/fetchling",
            "http://example.com/",
        ])
        .unwrap();
        match out {
            ParseOutcome::Run(c) => assert_eq!(c.urls, vec!["http://example.com/"]),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn keeps_host_path_without_program_name() {
        let out = parse_args(["example.com/path"]).unwrap();
        match out {
            ParseOutcome::Run(c) => assert_eq!(c.urls, vec!["example.com/path"]),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn rejects_http2() {
        let err = parse_args(["fetchling", "--http2", "http://x"]).unwrap_err();
        assert!(matches!(err, Error::DeferredOption(_)));
    }

    #[test]
    fn certificate_type_asn1_and_encodings_parse() {
        let out = parse_args([
            "fetchling",
            "--certificate-type=ASN1",
            "--private-key-type=asn1",
            "--local-encoding=ISO-8859-1",
            "--remote-encoding=UTF-8",
            "http://x",
        ])
        .unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert_eq!(c.certificate_type, "ASN1");
                assert_eq!(c.private_key_type, "asn1");
                assert_eq!(c.local_encoding.as_deref(), Some("ISO-8859-1"));
                assert_eq!(c.remote_encoding.as_deref(), Some("UTF-8"));
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn force_progress_and_netrc_file_parse() {
        let out = parse_args([
            "fetchling",
            "--force-progress",
            "--netrc-file=/tmp/custom.netrc",
            "http://x",
        ])
        .unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(c.show_progress);
                assert_eq!(c.netrc_file.as_deref(), Some("/tmp/custom.netrc"));
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn collision_flags_parse() {
        let out = parse_args(["fetchling", "--clobber", "http://x"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(!c.unique_names);
                assert!(!c.no_clobber);
            }
            _ => panic!("expected run"),
        }
        let out = parse_args(["fetchling", "--no-clobber", "http://x"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(c.no_clobber);
                assert!(!c.unique_names);
            }
            _ => panic!("expected run"),
        }
        let out = parse_args(["fetchling", "--unique-names", "http://x"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(c.unique_names);
                assert!(!c.no_clobber);
            }
            _ => panic!("expected run"),
        }
        let out = parse_args(["fetchling", "-nc", "http://x"]).unwrap();
        match out {
            ParseOutcome::Run(c) => {
                assert!(c.no_clobber);
                assert!(!c.unique_names);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn version_long_vs_short() {
        assert!(matches!(
            parse_args(["fetchling", "--version"]).unwrap(),
            ParseOutcome::Version
        ));
        assert!(matches!(
            parse_args(["fetchling", "-V"]).unwrap(),
            ParseOutcome::VersionShort
        ));
    }

    #[test]
    fn help_flag() {
        assert!(matches!(
            parse_args(["fetchling", "--help"]).unwrap(),
            ParseOutcome::Help
        ));
        assert!(matches!(
            parse_args(["fetchling", "-h"]).unwrap(),
            ParseOutcome::Help
        ));
    }

    #[test]
    fn config_flag_loads_file() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("fetchling-cli-rc-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "quota = 2k").unwrap();
        }
        let out = parse_args([
            "fetchling",
            &format!("--config={}", path.display()),
            "http://x",
        ])
        .unwrap();
        match out {
            ParseOutcome::Run(c) => assert_eq!(c.quota, Some(2048)),
            _ => panic!("expected run"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn no_config_ignores_defaults() {
        let out = parse_args(["fetchling", "--no-config", "http://x"]).unwrap();
        match out {
            ParseOutcome::Run(c) => assert!(c.no_config),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn missing_config_file_errors() {
        let err = parse_args([
            "fetchling",
            "--config=/nonexistent/fetchling-rc-missing",
            "http://x",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("cannot read config file"));
    }
}
