//! Cookie jar helpers (Netscape cookie file compatible subset).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use cookie_store::CookieStore;
use fetchling_core::{Error, Result};
use url::Url;

/// Netscape-tab cookie file subset.
///
/// Used internally by [`crate::HttpClient`]. Load and save skip `#` comment
/// lines and short lines; [`Self::save`] writes a Netscape header and includes
/// session cookies only when `keep_session` is true.
#[derive(Debug, Default)]
pub struct Jar {
    store: CookieStore,
}

impl Jar {
    /// Empty jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load cookies from a Netscape-tab file at `path`.
    ///
    /// Lines starting with `#`, empty lines, and rows with fewer than seven
    /// tab-separated fields are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read, or [`Error::Parse`]
    /// when a cookie URL cannot be built.
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mut store = CookieStore::default();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            // domain flag path secure expiry name value
            let parts: Vec<_> = line.split('\t').collect();
            if parts.len() < 7 {
                continue;
            }
            let domain = parts[0];
            let path_s = parts[2];
            let secure = parts[3].eq_ignore_ascii_case("TRUE");
            let name = parts[5];
            let value = parts[6];
            let host = domain.trim_start_matches('.');
            if host.is_empty() {
                continue;
            }
            let mut cookie_str = format!("{name}={value}; Domain={domain}; Path={path_s}");
            if secure {
                cookie_str.push_str("; Secure");
            }
            let scheme = if secure { "https" } else { "http" };
            let url = Url::parse(&format!("{scheme}://{host}{path_s}"))
                .map_err(|e| Error::Parse(format!("cookie url: {e}")))?;
            let _ = store.parse(&cookie_str, &url);
        }
        Ok(Self { store })
    }

    /// Write cookies to `path` in Netscape format.
    ///
    /// The file starts with a Netscape cookie-file header. Session cookies are
    /// written only when `keep_session` is true.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be created or written.
    pub fn save(&self, path: &Path, keep_session: bool) -> Result<()> {
        let mut f = File::create(path)?;
        writeln!(
            f,
            "# Netscape HTTP Cookie File\n# https://curl.se/docs/http-cookies.html"
        )?;
        for cookie in self.store.iter_any() {
            let expiry = match &cookie.expires {
                cookie_store::CookieExpiration::SessionEnd => {
                    if !keep_session {
                        continue;
                    }
                    "0".to_string()
                }
                cookie_store::CookieExpiration::AtUtc(t) => t.unix_timestamp().to_string(),
            };
            let domain = cookie.domain().unwrap_or("");
            // Host-only cookies may lack a domain; Netscape lines need a host.
            if domain.is_empty() {
                continue;
            }
            let include = if domain.starts_with('.') {
                "TRUE"
            } else {
                "FALSE"
            };
            let path_s = cookie.path().unwrap_or("/");
            let secure = if cookie.secure().unwrap_or(false) {
                "TRUE"
            } else {
                "FALSE"
            };
            writeln!(
                f,
                "{domain}\t{include}\t{path_s}\t{secure}\t{expiry}\t{}\t{}",
                cookie.name(),
                cookie.value()
            )?;
        }
        Ok(())
    }

    /// `Cookie` header value for `url`, if the jar has matching cookies.
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        let cookies: Vec<_> = self
            .store
            .get_request_values(url)
            .map(|(n, v)| format!("{n}={v}"))
            .collect();
        if cookies.is_empty() {
            None
        } else {
            Some(cookies.join("; "))
        }
    }

    /// Parse `Set-Cookie` header values and store cookies for `url`.
    pub fn store_from_headers<'a, I>(&mut self, url: &Url, headers: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        for h in headers {
            let _ = self.store.parse(h, url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar_path(suffix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fetchling-http-jar-{suffix}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn jar_load_skips_comments_and_short_lines() {
        let path = jar_path("load");
        let text = "\
# Netscape HTTP Cookie File

short\tline
.\tFALSE\t/\tFALSE\t0\tskip\tx
example.com\tFALSE\t/\tFALSE\t0\tid\tabc
";
        std::fs::write(&path, text).unwrap();
        let jar = Jar::load(&path).unwrap();
        let url = Url::parse("http://example.com/").unwrap();
        assert_eq!(jar.cookie_header(&url).as_deref(), Some("id=abc"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jar_save_session_and_roundtrip() {
        let mut jar = Jar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.store_from_headers(&url, ["sid=1; Domain=example.com; Path=/"]);
        let path = jar_path("save");
        jar.save(&path, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# Netscape HTTP Cookie File"));
        assert!(!text.contains("sid"));
        jar.save(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("sid"));
        let loaded = Jar::load(&path).unwrap();
        assert_eq!(loaded.cookie_header(&url).as_deref(), Some("sid=1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jar_cookie_header_from_set_cookie() {
        let mut jar = Jar::new();
        let url = Url::parse("http://example.com/page").unwrap();
        jar.store_from_headers(&url, ["id=abc; Path=/"]);
        assert_eq!(jar.cookie_header(&url).as_deref(), Some("id=abc"));
        assert!(jar
            .cookie_header(&Url::parse("http://other.com/").unwrap())
            .is_none());
    }
}
