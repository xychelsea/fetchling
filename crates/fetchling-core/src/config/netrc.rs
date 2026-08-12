//! `.netrc` parsing and credential lookup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Login/password pair from a `.netrc` machine or default entry.
#[derive(Debug, Clone, Default)]
pub struct NetrcEntry {
    /// `login` token value.
    pub login: Option<String>,
    /// `password` token value.
    pub password: Option<String>,
}

/// Parsed `.netrc` file (per-machine entries plus optional `default`).
#[derive(Debug, Clone, Default)]
pub struct Netrc {
    machines: HashMap<String, NetrcEntry>,
    default: Option<NetrcEntry>,
}

impl Netrc {
    /// Look up credentials for `host`, falling back to the `default` entry.
    pub fn lookup(&self, host: &str) -> Option<&NetrcEntry> {
        self.machines.get(host).or(self.default.as_ref())
    }
}

/// Default path `$HOME/.netrc`, if `HOME` is set.
pub fn default_netrc_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".netrc"))
}

/// Load and parse a `.netrc` file; warn on Unix when the file is group/world-readable.
///
/// # Errors
///
/// Returns [`Error::Io`](crate::Error::Io) if the file cannot be read, or
/// [`Error::Parse`](crate::Error::Parse) if the contents are invalid.
pub fn load_netrc(path: &Path) -> Result<Netrc> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "fetchling: warning: netrc {} is group- or world-readable (mode {mode:04o}); credentials may be exposed",
                    path.display()
                );
            }
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("netrc {}: {e}", path.display()),
        ))
    })?;
    parse_netrc(&text)
}

/// Parse `.netrc` text into a [`Netrc`] map.
///
/// # Errors
///
/// Returns [`Error::Parse`](crate::Error::Parse) for malformed tokens (for
/// example `machine` without a host).
///
/// # Examples
///
/// ```
/// use fetchling_core::parse_netrc;
///
/// let netrc = parse_netrc(
///     "machine example.com\n  login alice\n  password secret\ndefault\n  login anon\n",
/// )
/// .unwrap();
/// assert_eq!(
///     netrc.lookup("example.com").unwrap().login.as_deref(),
///     Some("alice")
/// );
/// assert_eq!(netrc.lookup("other.org").unwrap().login.as_deref(), Some("anon"));
/// ```
pub fn parse_netrc(text: &str) -> Result<Netrc> {
    let mut netrc = Netrc::default();
    let mut tokens = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for t in line.split_whitespace() {
            tokens.push(t.to_string());
        }
    }

    let mut i = 0;
    let mut current_machine: Option<String> = None;
    let mut is_default = false;
    let mut entry = NetrcEntry::default();

    let flush = |netrc: &mut Netrc,
                 current_machine: &mut Option<String>,
                 is_default: &mut bool,
                 entry: &mut NetrcEntry| {
        if *is_default {
            netrc.default = Some(std::mem::take(entry));
            *is_default = false;
        } else if let Some(m) = current_machine.take() {
            netrc.machines.insert(m, std::mem::take(entry));
        }
    };

    while i < tokens.len() {
        match tokens[i].as_str() {
            "machine" => {
                flush(
                    &mut netrc,
                    &mut current_machine,
                    &mut is_default,
                    &mut entry,
                );
                i += 1;
                let host = tokens
                    .get(i)
                    .ok_or_else(|| Error::Parse("netrc: machine missing host".into()))?;
                current_machine = Some(host.clone());
                is_default = false;
                entry = NetrcEntry::default();
            }
            "default" => {
                flush(
                    &mut netrc,
                    &mut current_machine,
                    &mut is_default,
                    &mut entry,
                );
                is_default = true;
                current_machine = None;
                entry = NetrcEntry::default();
            }
            "login" => {
                i += 1;
                let v = tokens
                    .get(i)
                    .ok_or_else(|| Error::Parse("netrc: login missing value".into()))?;
                entry.login = Some(v.clone());
            }
            "password" => {
                i += 1;
                let v = tokens
                    .get(i)
                    .ok_or_else(|| Error::Parse("netrc: password missing value".into()))?;
                entry.password = Some(v.clone());
            }
            "account" | "macdef" => {
                i += 1;
                if tokens[i - 1] == "macdef" {
                    while i < tokens.len() && tokens[i] != "machine" && tokens[i] != "default" {
                        i += 1;
                    }
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    flush(
        &mut netrc,
        &mut current_machine,
        &mut is_default,
        &mut entry,
    );
    Ok(netrc)
}

/// Load netrc (from `netrc_file` or `$HOME/.netrc`) and return `(login, password)` for `host`.
pub fn lookup_credentials(host: &str, netrc_file: Option<&str>) -> Option<(String, String)> {
    let path = if let Some(p) = netrc_file {
        PathBuf::from(p)
    } else {
        default_netrc_path()?
    };
    let netrc = load_netrc(&path).ok()?;
    let entry = netrc.lookup(host)?;
    let login = entry.login.clone()?;
    let password = entry.password.clone().unwrap_or_default();
    Some((login, password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn parse_machine_and_default() {
        let text = r#"
# comment
machine example.com
  login alice
  password secret
default
  login anon
  password guest
"#;
        let n = parse_netrc(text).unwrap();
        let e = n.lookup("example.com").unwrap();
        assert_eq!(e.login.as_deref(), Some("alice"));
        assert_eq!(e.password.as_deref(), Some("secret"));
        let d = n.lookup("other.org").unwrap();
        assert_eq!(d.login.as_deref(), Some("anon"));
        assert_eq!(d.password.as_deref(), Some("guest"));
    }

    #[test]
    fn parse_errors_missing_values() {
        assert!(parse_netrc("machine")
            .unwrap_err()
            .to_string()
            .contains("machine missing host"));
        assert!(parse_netrc("machine h\nlogin")
            .unwrap_err()
            .to_string()
            .contains("login missing value"));
        assert!(parse_netrc("machine h\npassword")
            .unwrap_err()
            .to_string()
            .contains("password missing value"));
    }

    #[test]
    fn macdef_skipped_until_next_machine() {
        let text = r#"
machine example.com
  login alice
  password secret
  macdef init
  this is ignored
  so is this
machine other.org
  login bob
  password x
"#;
        let n = parse_netrc(text).unwrap();
        assert_eq!(
            n.lookup("example.com").unwrap().login.as_deref(),
            Some("alice")
        );
        assert_eq!(n.lookup("other.org").unwrap().login.as_deref(), Some("bob"));
    }

    #[test]
    fn lookup_none_without_match_or_default() {
        let n = parse_netrc("machine example.com\n  login u\n  password p\n").unwrap();
        assert!(n.lookup("missing.org").is_none());
    }

    #[test]
    fn lookup_credentials_from_file() {
        let dir = TempDir::new("fetchling-netrc");
        let path = dir.write(
            "netrc",
            "machine example.com\n  login alice\n  password secret\nmachine bare.com\n  login only\n",
        );
        let creds = lookup_credentials("example.com", Some(path.to_str().unwrap())).unwrap();
        assert_eq!(creds, ("alice".into(), "secret".into()));

        let creds = lookup_credentials("bare.com", Some(path.to_str().unwrap())).unwrap();
        assert_eq!(creds, ("only".into(), "".into()));

        assert!(lookup_credentials("missing.org", Some(path.to_str().unwrap())).is_none());
    }
}
