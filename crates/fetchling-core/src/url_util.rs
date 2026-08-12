//! URL parsing and normalization helpers.

use url::Url;

use crate::{Error, Result};

/// Parsed URL used throughout fetchling (scheme defaults to `http` for bare hosts).
#[derive(Debug, Clone)]
pub struct FetchUrl {
    /// Underlying [`url::Url`].
    pub url: Url,
}

impl FetchUrl {
    /// Parse `input`, allowing non-ASCII (IRI) characters.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Url`](crate::Error::Url) when the string is not a valid URL.
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_iri(input, true)
    }

    /// Parse `input`. When `allow_iri` is false, reject non-ASCII strings.
    ///
    /// # Errors
    ///
    /// - [`Error::Parse`](crate::Error::Parse) if `allow_iri` is false and `input`
    ///   contains non-ASCII characters
    /// - [`Error::Url`](crate::Error::Url) if the string is not a valid URL
    pub fn parse_iri(input: &str, allow_iri: bool) -> Result<Self> {
        if !allow_iri && !input.is_ascii() {
            return Err(Error::Parse(format!(
                "non-ASCII URL rejected with --no-iri: {input}"
            )));
        }
        let url = if input.contains("://") {
            Url::parse(input)?
        } else {
            Url::parse(&format!("http://{input}"))?
        };
        Ok(Self { url })
    }

    /// URL scheme (`http`, `https`, `ftp`, …).
    pub fn scheme(&self) -> &str {
        self.url.scheme()
    }

    /// Host component, if any.
    pub fn host_str(&self) -> Option<&str> {
        self.url.host_str()
    }

    /// Path component.
    pub fn path(&self) -> &str {
        self.url.path()
    }

    /// Full URL string.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }
}

/// Normalize a URL string (IRIs allowed). Prefers a clearer error message than bare parse.
///
/// # Errors
///
/// Returns [`Error::Parse`](crate::Error::Parse) with a `bad URL '…'` message when
/// parsing fails.
pub fn normalize_url(input: &str) -> Result<FetchUrl> {
    normalize_url_iri(input, true)
}

/// Normalize a URL string with optional IRI rejection.
///
/// # Errors
///
/// Returns [`Error::Parse`](crate::Error::Parse) when the URL is invalid or when
/// `allow_iri` is false and `input` is non-ASCII.
pub fn normalize_url_iri(input: &str, allow_iri: bool) -> Result<FetchUrl> {
    FetchUrl::parse_iri(input, allow_iri)
        .map_err(|e| Error::Parse(format!("bad URL '{input}': {e}")))
}

/// Remove query variables. `None` leaves the URL unchanged. `Some([])` strips the
/// entire query. `Some(names)` removes only those keys (case-sensitive).
pub fn strip_query_vars(url: &Url, vars: Option<&[String]>) -> Url {
    let Some(vars) = vars else {
        return url.clone();
    };
    let mut u = url.clone();
    if vars.is_empty() {
        u.set_query(None);
        return u;
    }
    let filtered: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !vars.iter().any(|v| v == k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if filtered.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(&filtered);
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host() {
        let u = FetchUrl::parse("example.com/a").unwrap();
        assert_eq!(u.scheme(), "http");
        assert_eq!(u.host_str(), Some("example.com"));
        assert_eq!(u.path(), "/a");
    }

    #[test]
    fn scheme_present_unchanged() {
        let u = FetchUrl::parse("https://example.com/a").unwrap();
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.as_str(), "https://example.com/a");
    }

    #[test]
    fn no_iri_rejects_non_ascii() {
        assert!(FetchUrl::parse_iri("http://example.com/café", false).is_err());
        assert!(FetchUrl::parse_iri("http://example.com/cafe", false).is_ok());
    }

    #[test]
    fn normalize_url_error_message() {
        let err = normalize_url("http://").unwrap_err();
        assert!(err.to_string().contains("bad URL"));
        let err = normalize_url_iri("http://example.com/café", false).unwrap_err();
        assert!(err.to_string().contains("bad URL"));
    }

    #[test]
    fn strip_query_vars_all_and_selected() {
        let u = Url::parse("http://ex/a?x=1&y=2&z=3").unwrap();
        let all = strip_query_vars(&u, Some(&[]));
        assert!(all.query().is_none());
        let some = strip_query_vars(&u, Some(&["y".into()]));
        let q = some.query().unwrap();
        assert!(q.contains("x=1"));
        assert!(q.contains("z=3"));
        assert!(!q.contains("y=2"));
        assert_eq!(strip_query_vars(&u, None).as_str(), u.as_str());
    }
}
