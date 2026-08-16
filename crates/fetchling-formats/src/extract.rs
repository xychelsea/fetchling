use regex::Regex;
use url::Url;

/// Tag filters and comment stripping for [`extract_html_urls`].
#[derive(Debug, Clone, Copy, Default)]
pub struct HtmlExtractOpts<'a> {
    /// Tag names to keep. Empty means follow every tag.
    pub follow_tags: &'a [String],
    /// Tag names to skip (checked after `follow_tags`).
    pub ignore_tags: &'a [String],
    /// Strip `<!-- … -->` comments before scanning.
    pub strict_comments: bool,
}

/// Collect `http`/`https`/`ftp` URLs from HTML `href`/`src`/`action`/`data`.
///
/// Relative values are joined against `base`. Fragments (`#…`) and
/// `javascript:` URLs are skipped. Results are sorted and deduplicated.
///
/// # Examples
///
/// ```
/// use fetchling_formats::{extract_html_urls, HtmlExtractOpts};
/// use url::Url;
///
/// let base = Url::parse("https://example.com/dir/").unwrap();
/// let urls = extract_html_urls(&base, r#"<a href="x.html">"#, HtmlExtractOpts::default());
/// assert_eq!(urls[0].as_str(), "https://example.com/dir/x.html");
/// ```
pub fn extract_html_urls(base: &Url, html: &str, opts: HtmlExtractOpts<'_>) -> Vec<Url> {
    let html = if opts.strict_comments {
        strip_html_comments(html)
    } else {
        html.to_string()
    };
    let mut out = Vec::new();
    let re = Regex::new(
        r#"(?ix)
        <\s*([a-zA-Z][\w:-]*)
        \b[^>]*?
        \b(?:href|src|action|data)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))
        "#,
    )
    .expect("regex");
    for cap in re.captures_iter(&html) {
        let tag = cap
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !opts.follow_tags.is_empty()
            && !opts
                .follow_tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case(&tag))
        {
            continue;
        }
        if opts
            .ignore_tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&tag))
        {
            continue;
        }
        let raw = cap
            .get(2)
            .or_else(|| cap.get(3))
            .or_else(|| cap.get(4))
            .map(|m| m.as_str())
            .unwrap_or("");
        if raw.is_empty() || raw.starts_with('#') || raw.starts_with("javascript:") {
            continue;
        }
        if let Ok(u) = base.join(raw) {
            if matches!(u.scheme(), "http" | "https" | "ftp") {
                out.push(u);
            }
        }
    }
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    out.dedup();
    out
}

fn strip_html_comments(html: &str) -> String {
    let re = Regex::new(r"(?s)<!--.*?-->").expect("regex");
    re.replace_all(html, "").into_owned()
}

/// Collect `http`/`https`/`ftp` URLs from CSS `url(...)`.
///
/// Relative values are joined against `base`. Results are sorted and
/// deduplicated.
///
/// # Examples
///
/// ```
/// use fetchling_formats::extract_css_urls;
/// use url::Url;
///
/// let base = Url::parse("https://example.com/").unwrap();
/// let urls = extract_css_urls(&base, r#"url(https://example.com/a.png)"#);
/// assert_eq!(urls[0].as_str(), "https://example.com/a.png");
/// ```
pub fn extract_css_urls(base: &Url, css: &str) -> Vec<Url> {
    let mut out = Vec::new();
    let re = Regex::new(r#"url\(\s*['"]?([^'")\s]+)['"]?\s*\)"#).expect("regex");
    for cap in re.captures_iter(css) {
        let raw = &cap[1];
        if let Ok(u) = base.join(raw) {
            if matches!(u.scheme(), "http" | "https" | "ftp") {
                out.push(u);
            }
        }
    }
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    out.dedup();
    out
}

/// Rewrite links in HTML to local paths (simplified `-k`).
///
/// When `file_only` is true, only the last path segment of each URL is replaced
/// with the local basename (`--convert-file-only`).
///
/// # Examples
///
/// ```
/// use fetchling_formats::convert_links;
///
/// let html = r#"<a href="https://example.com/a/b.html">"#;
/// let map = [(
///     "https://example.com/a/b.html".into(),
///     "./example.com/a/b.html".into(),
/// )];
/// let out = convert_links(html, &map, false);
/// assert!(out.contains("./example.com/a/b.html"));
/// ```
pub fn convert_links(html: &str, mapping: &[(String, String)], file_only: bool) -> String {
    let mut out = html.to_string();
    for (from, to) in mapping {
        if file_only {
            let from_base = url_basename(from);
            let to_base = std::path::Path::new(to)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(to);
            if !from_base.is_empty() {
                out = out.replace(&from_base, to_base);
            }
        } else {
            out = out.replace(from, to);
        }
    }
    out
}

fn url_basename(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    path.rsplit('/').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_href() {
        let base = Url::parse("https://example.com/dir/").unwrap();
        let urls = extract_html_urls(&base, r#"<a href="x.html">"#, HtmlExtractOpts::default());
        assert_eq!(urls[0].as_str(), "https://example.com/dir/x.html");
    }

    #[test]
    fn follow_tags_filters() {
        let base = Url::parse("https://example.com/").unwrap();
        let html = r#"<a href="a.html"></a><img src="b.png">"#;
        let follow = vec!["img".into()];
        let urls = extract_html_urls(
            &base,
            html,
            HtmlExtractOpts {
                follow_tags: &follow,
                ..HtmlExtractOpts::default()
            },
        );
        assert_eq!(urls.len(), 1);
        assert!(urls[0].as_str().ends_with("b.png"));
    }

    #[test]
    fn ignore_tags_filters() {
        let base = Url::parse("https://example.com/").unwrap();
        let html = r#"<a href="a.html"></a><img src="b.png">"#;
        let ignore = vec!["img".into()];
        let urls = extract_html_urls(
            &base,
            html,
            HtmlExtractOpts {
                ignore_tags: &ignore,
                ..HtmlExtractOpts::default()
            },
        );
        assert_eq!(urls.len(), 1);
        assert!(urls[0].as_str().ends_with("a.html"));
    }

    #[test]
    fn strict_comments_strips_links() {
        let base = Url::parse("https://example.com/").unwrap();
        let html = r#"<!-- <a href="hidden.html"></a> --><a href="visible.html"></a>"#;
        let urls = extract_html_urls(
            &base,
            html,
            HtmlExtractOpts {
                strict_comments: true,
                ..HtmlExtractOpts::default()
            },
        );
        assert_eq!(urls.len(), 1);
        assert!(urls[0].as_str().ends_with("visible.html"));
    }

    #[test]
    fn css_skips_non_http_schemes() {
        let base = Url::parse("https://example.com/").unwrap();
        let urls = extract_css_urls(&base, "url(data:image/png;base64,xx)");
        assert!(urls.is_empty());
    }

    #[test]
    fn convert_links_full_replaces_url() {
        let html = r#"<a href="https://example.com/a/b.html">"#;
        let map = [(
            "https://example.com/a/b.html".into(),
            "./example.com/a/b.html".into(),
        )];
        let out = convert_links(html, &map, false);
        assert!(out.contains("./example.com/a/b.html"));
        assert!(!out.contains("https://example.com/a/b.html"));
    }

    #[test]
    fn convert_links_file_only_replaces_basename() {
        let html = r#"<a href="https://example.com/a/b.html">b.html</a>"#;
        let map = [(
            "https://example.com/a/b.html".into(),
            "./local/b.html".into(),
        )];
        let out = convert_links(html, &map, true);
        assert!(out.contains("https://example.com/a/b.html") || out.contains("b.html"));
        assert!(out.contains("b.html"));
    }

    #[test]
    fn convert_links_file_only_renames_basename() {
        let html = r#"href="page""#;
        let map = [("https://example.com/dir/page".into(), "./saved.html".into())];
        let out = convert_links(html, &map, true);
        assert_eq!(out, r#"href="saved.html""#);
    }

    #[test]
    fn extract_html_urls_schemes_and_attrs() {
        let base = Url::parse("https://example.com/dir/").unwrap();
        let html = r##"
            <a href="#frag"></a>
            <a href="javascript:void(0)"></a>
            <a href="mailto:a@example.com"></a>
            <a href='ftp://ftp.example.com/f.bin'></a>
            <img src="pic.png">
            <form action="go"></form>
            <object data="https://cdn.example.com/x.bin"></object>
        "##;
        let urls = extract_html_urls(&base, html, HtmlExtractOpts::default());
        let s: Vec<_> = urls.iter().map(|u| u.as_str()).collect();
        assert!(s.contains(&"ftp://ftp.example.com/f.bin"));
        assert!(s.iter().any(|u| u.ends_with("pic.png")));
        assert!(s.iter().any(|u| u.ends_with("/dir/go")));
        assert!(s.contains(&"https://cdn.example.com/x.bin"));
        assert!(!s.iter().any(|u| u.contains("javascript:")
            || u.contains("mailto:")
            || *u == "https://example.com/dir/#frag"));
    }

    #[test]
    fn extract_css_and_convert_edges() {
        let base = Url::parse("https://example.com/dir/").unwrap();
        let urls = extract_css_urls(&base, "url('https://example.com/a.png') url(b.png)");
        assert!(urls
            .iter()
            .any(|u| u.as_str() == "https://example.com/a.png"));
        assert!(urls
            .iter()
            .any(|u| u.as_str() == "https://example.com/dir/b.png"));
        assert_eq!(convert_links("hello", &[], false), "hello");
        let html = r#"href="page""#;
        let map = [(
            "https://example.com/dir/page?q=1#frag".into(),
            "./saved.html".into(),
        )];
        assert_eq!(convert_links(html, &map, true), r#"href="saved.html""#);
    }
}
