use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct NetrcEntry {
    pub login: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Netrc {
    machines: HashMap<String, NetrcEntry>,
    default: Option<NetrcEntry>,
}

impl Netrc {
    pub fn lookup(&self, host: &str) -> Option<&NetrcEntry> {
        self.machines.get(host).or(self.default.as_ref())
    }
}

pub fn default_netrc_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".netrc"))
}

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
}
