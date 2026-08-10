//! Cookie jar helpers (Netscape cookie file compatible subset).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use cookie_store::CookieStore;
use fetchling_core::{Error, Result};
use url::Url;

#[derive(Debug, Default)]
pub struct Jar {
    store: CookieStore,
}

impl Jar {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn store_from_headers<'a, I>(&mut self, url: &Url, headers: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        for h in headers {
            let _ = self.store.parse(h, url);
        }
    }
}
