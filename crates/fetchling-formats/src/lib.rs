//! Metalink, WARC, robots.txt, and HTML/CSS/feed link extraction.
//!
//! # What this crate is (and is not)
//!
//! IS: in-memory parsers and writers for a retriever: HTML/CSS URL extraction
//! ([`extract_html_urls`], [`extract_css_urls`], [`HtmlExtractOpts`]), offline
//! link rewrite ([`convert_links`]), RSS/Atom/sitemap URLs, robots.txt
//! ([`Robots`]), Metalink 3/4 XML and HTTP `Link` headers, hash encode/decode,
//! and WARC 1.0 writing ([`WarcWriter`]). The public API is flat at the crate
//! root. Drive WARC via [`fetchling_core::Config`] fields set directly; CLI /
//! wget names in those field docs are compatibility aliases. All entry points
//! are synchronous.
//!
//! IS NOT: HTTP/FTP clients, fetching robots/metalink over the network,
//! recursive mirroring, HTML/CSS DOM parsing, robots `Allow` rules, or reading
//! `.warc` files. Recursion and I/O live in `fetchling-http` /
//! `fetchling-engine` or the caller. This crate does not re-export
//! [`fetchling_core::Config`] or [`fetchling_core::Error`].
//!
//! # Typical integration
//!
//! 1. Fetch bytes yourself (or via `fetchling-http`)
//! 2. Extract URLs with [`extract_html_urls`] / [`extract_css_urls`] / feed helpers
//! 3. Gate with [`Robots::parse`] / [`Robots::allows`]
//! 4. Parse Metalink ([`parse_metalink_doc`]) or `Link` headers ([`parse_link_headers`])
//! 5. Optionally [`WarcWriter::open`] and [`WarcWriter::write_request`] /
//!    [`WarcWriter::write_response`] / [`WarcWriter::write_resource`]
//!
//! # Areas
//!
//! - **Extraction** — [`HtmlExtractOpts`], [`extract_html_urls`],
//!   [`extract_css_urls`], [`convert_links`]
//! - **Feeds** — [`extract_rss_urls`], [`extract_atom_urls`],
//!   [`extract_sitemap_urls`]
//! - **Robots** — [`Robots`]
//! - **Metalink** — [`MetalinkDoc`], [`MetalinkFile`], [`MetalinkUrl`],
//!   [`MetalinkHash`], [`MetalinkMetaUrl`], [`MetalinkLink`],
//!   [`parse_metalink`], [`parse_metalink_doc`], [`parse_link_header`],
//!   [`parse_link_headers`], [`is_metalink_mediatype`], [`encode_hashes`],
//!   [`decode_hashes`]
//! - **WARC** — [`WarcWriter`], [`WarcWriteInfo`]
//!
//! # Examples
//!
//! Extract and rewrite HTML/CSS links (no network):
//!
//! ```
//! use fetchling_formats::{convert_links, extract_css_urls, extract_html_urls, HtmlExtractOpts};
//! use url::Url;
//!
//! let base = Url::parse("https://example.com/dir/").unwrap();
//! let urls = extract_html_urls(&base, r#"<a href="x.html">"#, HtmlExtractOpts::default());
//! assert_eq!(urls[0].as_str(), "https://example.com/dir/x.html");
//!
//! let css = extract_css_urls(&base, r#"url(https://example.com/a.png)"#);
//! assert_eq!(css[0].as_str(), "https://example.com/a.png");
//!
//! let html = r#"<a href="https://example.com/a/b.html">"#;
//! let map = [(
//!     "https://example.com/a/b.html".into(),
//!     "./example.com/a/b.html".into(),
//! )];
//! let out = convert_links(html, &map, false);
//! assert!(out.contains("./example.com/a/b.html"));
//! ```
//!
//! Parse robots.txt and Metalink (no network):
//!
//! ```
//! use fetchling_formats::{parse_metalink, Robots};
//! use url::Url;
//!
//! let robots = Robots::parse("User-agent: *\nDisallow: /private\n");
//! assert!(!robots.allows("fetchling", &Url::parse("http://ex/private/a").unwrap()));
//! assert!(robots.allows("fetchling", &Url::parse("http://ex/public").unwrap()));
//!
//! let xml = r#"<?xml version="1.0"?>
//! <metalink xmlns="urn:ietf:params:xml:ns:metalink">
//!   <file name="hello.txt">
//!     <url>http://example.com/hello.txt</url>
//!     <hash type="sha-256">abc</hash>
//!   </file>
//! </metalink>"#;
//! let files = parse_metalink(xml).unwrap();
//! assert_eq!(files[0].name, "hello.txt");
//! assert_eq!(files[0].sha256(), Some("abc"));
//! ```
//!
//! Write a WARC response (does not run; creates files):
//!
//! ```no_run
//! use fetchling_core::Config;
//! use fetchling_formats::WarcWriter;
//!
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! cfg.warc_file = Some("example.warc".into());
//! let mut warc = WarcWriter::open(&cfg).unwrap().unwrap();
//! let _ = warc
//!     .write_response(
//!         "https://example.com/file.bin",
//!         b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody",
//!         200,
//!         Some("application/octet-stream"),
//!         None,
//!         None,
//!     )
//!     .unwrap();
//! ```

#![warn(missing_docs)]

mod extract;
mod feeds;
mod metalink;
mod robots;
mod warc;

pub use extract::{convert_links, extract_css_urls, extract_html_urls, HtmlExtractOpts};
pub use feeds::{extract_atom_urls, extract_rss_urls, extract_sitemap_urls};
pub use metalink::{
    decode_hashes, encode_hashes, is_metalink_mediatype, parse_link_header, parse_link_headers,
    parse_metalink, parse_metalink_doc, MetalinkDoc, MetalinkFile, MetalinkHash, MetalinkLink,
    MetalinkMetaUrl, MetalinkUrl,
};
pub use robots::Robots;
pub use warc::{WarcWriteInfo, WarcWriter};
