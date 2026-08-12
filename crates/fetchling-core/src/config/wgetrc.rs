//! Minimal `.wgetrc` or `-e` command application.

use std::path::{Path, PathBuf};

use crate::{parse_bytes, parse_seconds, parse_tries, Error, Result};

use super::Config;

/// Apply a single wgetrc-style command (`key = value` or `key = on/off`).
///
/// # Syntax
///
/// - Blank lines and `#` comments are ignored
/// - Keys are lowercased and `_` / `-` are stripped before matching
///   (so `dns_timeout`, `dns-timeout`, and `dnstimeout` are equivalent)
/// - Booleans accept `on` / `off` / `yes` / `no` / `true` / `false` / `1` / `0`
///
/// # Supported keys
///
/// `verbose`, `quiet`, `debug`, `recursive`, `timestamping`, `continue` /
/// `alwaysrest`, `noclobber`, `spider`, `robots` (accepted with a warning; not
/// applied), `tries` / `numtries`, `timeout`, `dns_timeout` / `dnstimeout`,
/// `connecttimeout`, `readtimeout`, `wait`, `waitretry`, `limitrate`,
/// `dirprefix`, `useragent`, `logfile`, `outputdocument`, `httpuser`,
/// `httppassword` / `httppasswd`, `ftpuser`, `ftppassword` / `ftppasswd`,
/// `user`, `password`, `checkcertificate`, `httpsonly`, `hsts`, `cookies`,
/// `convertlinks`, `pagerequisites`, `spanhosts`, `noparent`, `mirror`,
/// `reclevel`, `useproxy`, `passiveftp`, `netrc`, `iri`, `localencoding`,
/// `remoteencoding`, `adjustextension` / `htmlextension`, `contentdisposition`,
/// `maxredirect`, `quota`, `bindaddress`, `base`, `saveheaders`,
/// `useservertimestamps`, `ifmodifiedsince`
///
/// # Errors
///
/// Returns [`Error::Parse`](crate::Error::Parse) for a missing `=`, unknown key,
/// or invalid value.
///
/// # Examples
///
/// ```
/// use fetchling_core::{apply_wgetrc_command, Config};
///
/// let mut cfg = Config::default();
/// apply_wgetrc_command(&mut cfg, "quiet = on").unwrap();
/// assert!(cfg.quiet);
/// ```
pub fn apply_wgetrc_command(cfg: &mut Config, command: &str) -> Result<()> {
    let command = command.trim();
    if command.is_empty() || command.starts_with('#') {
        return Ok(());
    }
    let (key, value) = match command.split_once('=') {
        Some((k, v)) => (k.trim(), v.trim()),
        None => {
            return Err(Error::Parse(format!(
                "wgetrc command missing '=': {command}"
            )))
        }
    };
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");

    match key.as_str() {
        "verbose" => cfg.verbose = parse_bool(value)?,
        "quiet" => cfg.quiet = parse_bool(value)?,
        "debug" => cfg.debug = parse_bool(value)?,
        "recursive" => cfg.recursive = parse_bool(value)?,
        "timestamping" => cfg.timestamping = parse_bool(value)?,
        "continue" | "alwaysrest" => cfg.continue_download = parse_bool(value)?,
        "noclobber" => {
            cfg.no_clobber = parse_bool(value)?;
            if cfg.no_clobber {
                cfg.unique_names = false;
            }
        }
        "spider" => cfg.spider = parse_bool(value)?,
        "robots" => {
            // wget supports `robots = off` to skip robots.txt; fetchling still always
            // consults robots during recursive / page-requisite retrieval.
            eprintln!(
                "fetchling: warning: wgetrc 'robots = {value}' is not applied (robots.txt is always enforced when recursing)"
            );
        }
        "tries" | "numtries" => cfg.tries = parse_tries(value)?,
        "timeout" => cfg.apply_timeout(parse_seconds(value)?),
        "dns_timeout" | "dnstimeout" => cfg.dns_timeout = Some(parse_seconds(value)?),
        "connecttimeout" => cfg.connect_timeout = Some(parse_seconds(value)?),
        "readtimeout" => cfg.read_timeout = Some(parse_seconds(value)?),
        "wait" => cfg.wait = parse_seconds(value)?,
        "waitretry" => cfg.waitretry = parse_seconds(value)?,
        "limitrate" => cfg.limit_rate = Some(parse_bytes(value)?.get()),
        "dirprefix" => cfg.directory_prefix = value.to_string(),
        "useragent" => cfg.user_agent = value.to_string(),
        "logfile" => cfg.logfile = Some(value.to_string()),
        "outputdocument" => cfg.output_document = Some(value.to_string()),
        "httpuser" => cfg.http_user = Some(value.to_string()),
        "httppassword" | "httppasswd" => cfg.http_password = Some(value.to_string()),
        "ftpuser" => cfg.ftp_user = Some(value.to_string()),
        "ftppassword" | "ftppasswd" => cfg.ftp_password = Some(value.to_string()),
        "user" => cfg.user = Some(value.to_string()),
        "password" => cfg.password = Some(value.to_string()),
        "checkcertificate" => cfg.check_certificate = parse_bool(value)?,
        "httpsonly" => cfg.https_only = parse_bool(value)?,
        "hsts" => cfg.hsts = parse_bool(value)?,
        "cookies" => cfg.cookies = parse_bool(value)?,
        "convertlinks" => cfg.convert_links = parse_bool(value)?,
        "pagerequisites" => cfg.page_requisites = parse_bool(value)?,
        "spanhosts" => cfg.span_hosts = parse_bool(value)?,
        "noparent" => cfg.no_parent = parse_bool(value)?,
        "mirror" if parse_bool(value)? => cfg.apply_mirror(),
        "reclevel" => {
            if value.eq_ignore_ascii_case("inf") {
                cfg.level = -1;
            } else {
                cfg.level = value
                    .parse()
                    .map_err(|_| Error::Parse(format!("bad reclevel: {value}")))?;
            }
        }
        "useproxy" => cfg.use_proxy = parse_bool(value)?,
        "passiveftp" => cfg.passive_ftp = parse_bool(value)?,
        "netrc" => cfg.netrc = parse_bool(value)?,
        "iri" => cfg.iri = parse_bool(value)?,
        "localencoding" => cfg.local_encoding = Some(value.to_string()),
        "remoteencoding" => cfg.remote_encoding = Some(value.to_string()),
        "adjustextension" | "htmlextension" => cfg.adjust_extension = parse_bool(value)?,
        "contentdisposition" => cfg.content_disposition = parse_bool(value)?,
        "maxredirect" => {
            cfg.max_redirect = value
                .parse()
                .map_err(|_| Error::Parse(format!("bad maxredirect: {value}")))?
        }
        "quota" => cfg.quota = Some(parse_bytes(value)?.get()),
        "bindaddress" => cfg.bind_address = Some(value.to_string()),
        "base" => cfg.base = Some(value.to_string()),
        "saveheaders" => cfg.save_headers = parse_bool(value)?,
        "useservertimestamps" => cfg.use_server_timestamps = parse_bool(value)?,
        "ifmodifiedsince" => cfg.if_modified_since = parse_bool(value)?,
        other => {
            return Err(Error::Parse(format!("unknown wgetrc command: {other}")));
        }
    }
    Ok(())
}

/// Load wgetrc files into `cfg`.
///
/// Search order when `config_file` is unset and `no_config` is false:
///
/// 1. `SYSTEM_WGETRC` if set, otherwise `/etc/wgetrc`
/// 2. `WGETRC` if set, otherwise `$HOME/.wgetrc`
///
/// Missing default files are skipped. When `config_file` is set, that path is
/// required and is the only file loaded. When `no_config` is true, nothing is
/// loaded.
///
/// # Errors
///
/// Returns [`Error::Parse`](crate::Error::Parse) if a required file cannot be
/// read or a line fails [`apply_wgetrc_command`].
pub fn load_wgetrc_files(cfg: &mut Config) -> Result<()> {
    if cfg.no_config {
        return Ok(());
    }
    if let Some(path) = cfg.config_file.clone() {
        return apply_wgetrc_file(cfg, Path::new(&path), true);
    }
    for path in default_wgetrc_paths() {
        apply_wgetrc_file(cfg, &path, false)?;
    }
    Ok(())
}

fn default_wgetrc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(p) = std::env::var("SYSTEM_WGETRC") {
        if !p.is_empty() {
            paths.push(PathBuf::from(p));
        }
    } else {
        paths.push(PathBuf::from("/etc/wgetrc"));
    }
    if let Ok(p) = std::env::var("WGETRC") {
        if !p.is_empty() {
            paths.push(PathBuf::from(p));
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".wgetrc"));
    }
    paths
}

fn apply_wgetrc_file(cfg: &mut Config, path: &Path, required: bool) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(Error::Parse(format!(
                "cannot read config file {}: {e}",
                path.display()
            )))
        }
    };
    for (lineno, line) in text.lines().enumerate() {
        apply_wgetrc_command(cfg, line)
            .map_err(|e| Error::Parse(format!("{}:{}: {e}", path.display(), lineno + 1)))?;
    }
    Ok(())
}

fn parse_bool(s: &str) -> Result<bool> {
    match s.to_ascii_lowercase().as_str() {
        "1" | "on" | "yes" | "true" => Ok(true),
        "0" | "off" | "no" | "false" => Ok(false),
        _ => Err(Error::Parse(format!("invalid boolean: {s}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let p = self.path.join(name);
            std::fs::write(&p, contents).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn set_verbose_off() {
        let mut c = Config::default();
        apply_wgetrc_command(&mut c, "verbose = off").unwrap();
        assert!(!c.verbose);
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let mut c = Config::default();
        apply_wgetrc_command(&mut c, "").unwrap();
        apply_wgetrc_command(&mut c, "   ").unwrap();
        apply_wgetrc_command(&mut c, "# quiet = on").unwrap();
        assert!(!c.quiet);
    }

    #[test]
    fn apply_keys_and_aliases() {
        let mut c = Config::default();
        apply_wgetrc_command(&mut c, "quiet = on").unwrap();
        assert!(c.quiet);

        apply_wgetrc_command(&mut c, "tries = 5").unwrap();
        assert_eq!(c.tries, 5);
        apply_wgetrc_command(&mut c, "numtries = 7").unwrap();
        assert_eq!(c.tries, 7);

        apply_wgetrc_command(&mut c, "timeout = 15").unwrap();
        assert_eq!(c.timeout, Some(15.0));
        assert_eq!(c.dns_timeout, Some(15.0));
        assert_eq!(c.connect_timeout, Some(15.0));
        assert_eq!(c.read_timeout, Some(15.0));

        apply_wgetrc_command(&mut c, "dnstimeout = 9").unwrap();
        assert_eq!(c.dns_timeout, Some(9.0));
        apply_wgetrc_command(&mut c, "dns-timeout = 11").unwrap();
        assert_eq!(c.dns_timeout, Some(11.0));

        apply_wgetrc_command(&mut c, "mirror = on").unwrap();
        assert!(c.recursive);
        assert_eq!(c.level, -1);
        assert!(c.timestamping);
        assert!(!c.remove_listing);

        c.unique_names = true;
        apply_wgetrc_command(&mut c, "noclobber = on").unwrap();
        assert!(c.no_clobber);
        assert!(!c.unique_names);

        apply_wgetrc_command(&mut c, "reclevel = inf").unwrap();
        assert_eq!(c.level, -1);
        apply_wgetrc_command(&mut c, "reclevel = 3").unwrap();
        assert_eq!(c.level, 3);

        apply_wgetrc_command(&mut c, "quota = 1k").unwrap();
        assert_eq!(c.quota, Some(1024));

        apply_wgetrc_command(&mut c, "httppasswd = secret").unwrap();
        assert_eq!(c.http_password.as_deref(), Some("secret"));
        apply_wgetrc_command(&mut c, "httppassword = other").unwrap();
        assert_eq!(c.http_password.as_deref(), Some("other"));
    }

    #[test]
    fn apply_negatives() {
        let mut c = Config::default();
        let err = apply_wgetrc_command(&mut c, "quiet on").unwrap_err();
        assert!(err.to_string().contains("missing '='"));

        let err = apply_wgetrc_command(&mut c, "notakey = on").unwrap_err();
        assert!(err.to_string().contains("unknown wgetrc command"));

        let err = apply_wgetrc_command(&mut c, "quiet = maybe").unwrap_err();
        assert!(err.to_string().contains("invalid boolean"));

        let err = apply_wgetrc_command(&mut c, "maxredirect = nope").unwrap_err();
        assert!(err.to_string().contains("bad maxredirect"));
    }

    #[test]
    fn load_required_missing_errors() {
        let mut c = Config {
            config_file: Some("/nonexistent/fetchling-wgetrc-test".into()),
            ..Config::default()
        };
        let err = load_wgetrc_files(&mut c).unwrap_err();
        assert!(err.to_string().contains("cannot read config file"));
    }

    #[test]
    fn load_explicit_config_file() {
        let dir = TempDir::new("fetchling-rc");
        let path = dir.write("wgetrc", "quiet = on\nquota = 1k\n");
        let mut c = Config {
            config_file: Some(path.display().to_string()),
            ..Config::default()
        };
        load_wgetrc_files(&mut c).unwrap();
        assert!(c.quiet);
        assert_eq!(c.quota, Some(1024));
    }

    #[test]
    fn no_config_skips_load() {
        let mut c = Config {
            no_config: true,
            config_file: Some("/nonexistent/fetchling-wgetrc-test".into()),
            ..Config::default()
        };
        load_wgetrc_files(&mut c).unwrap();
    }

    #[test]
    fn robots_setting_is_accepted_with_warning() {
        let mut c = Config::default();
        apply_wgetrc_command(&mut c, "robots = off").unwrap();
        // Behavior is unchanged (no config field); warning is emitted on stderr.
    }

    #[test]
    fn load_search_order_system_then_user() {
        let _guard = env_lock().lock().unwrap();
        let dir = TempDir::new("fetchling-wgetrc-env");
        let system = dir.write("system.wgetrc", "quiet = on\n");
        let user = dir.write("user.wgetrc", "tries = 3\n");

        let prev_system = std::env::var_os("SYSTEM_WGETRC");
        let prev_wgetrc = std::env::var_os("WGETRC");
        // SAFETY: serialized by env_lock; restored below.
        unsafe {
            std::env::set_var("SYSTEM_WGETRC", &system);
            std::env::set_var("WGETRC", &user);
        }

        let mut c = Config::default();
        c.config_file = None;
        c.no_config = false;
        let result = load_wgetrc_files(&mut c);

        // SAFETY: restore previous env under the same lock.
        unsafe {
            match prev_system {
                Some(v) => std::env::set_var("SYSTEM_WGETRC", v),
                None => std::env::remove_var("SYSTEM_WGETRC"),
            }
            match prev_wgetrc {
                Some(v) => std::env::set_var("WGETRC", v),
                None => std::env::remove_var("WGETRC"),
            }
        }

        result.unwrap();
        assert!(c.quiet);
        assert_eq!(c.tries, 3);
    }

    #[test]
    fn load_line_numbered_parse_error() {
        let dir = TempDir::new("fetchling-rc-err");
        let path = dir.write("bad.wgetrc", "quiet = on\nbadline\n");
        let mut c = Config {
            config_file: Some(path.display().to_string()),
            ..Config::default()
        };
        let err = load_wgetrc_files(&mut c).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(":2:"), "{msg}");
        assert!(msg.contains("missing '='"), "{msg}");
    }
}
