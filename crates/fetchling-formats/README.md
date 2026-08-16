# fetchling-formats

Metalink, WARC, robots.txt, and HTML/CSS/feed link extraction for building a
network retriever (used by
[fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: in-memory parsers and writers you call from your own client—extract URLs
from HTML/CSS and feeds, rewrite links offline, parse robots.txt and Metalink,
and write WARC 1.0 records. Set `fetchling_core::Config` fields directly for
WARC; CLI / wget names in field docs are compatibility aliases. Every entry
point is synchronous.

IS NOT: an HTTP or FTP client, network fetch of robots/metalink, recursive
mirroring, a full HTML/CSS DOM parser, robots `Allow` rules, or a WARC reader.
Those belong in other `fetchling-*` crates or in your own code. This crate does
not re-export `Config` or `Error`.

The public API is flat at the crate root (`fetchling_formats::extract_html_urls`,
`fetchling_formats::WarcWriter`, and so on).

## Typical integration

1. Fetch bytes yourself (or via `fetchling-http`)
2. Extract URLs with `extract_html_urls` / `extract_css_urls` / the feed helpers
3. Gate with `Robots::parse` / `Robots::allows`
4. Parse Metalink (`parse_metalink_doc`) or HTTP `Link` headers
5. Optionally `WarcWriter::open` and `write_request` / `write_response` /
   `write_resource`

```rust
use fetchling_formats::{
    extract_html_urls, parse_metalink, HtmlExtractOpts, Robots,
};
use url::Url;

let base = Url::parse("https://example.com/dir/").unwrap();
let urls = extract_html_urls(&base, r#"<a href="x.html">"#, HtmlExtractOpts::default());
assert_eq!(urls[0].as_str(), "https://example.com/dir/x.html");

let robots = Robots::parse("User-agent: *\nDisallow: /private\n");
assert!(!robots.allows("fetchling", &Url::parse("http://ex/private/a").unwrap()));

let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="hello.txt">
    <url>http://example.com/hello.txt</url>
    <hash type="sha-256">abc</hash>
  </file>
</metalink>"#;
let files = parse_metalink(xml).unwrap();
assert_eq!(files[0].name, "hello.txt");
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
