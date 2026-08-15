# fetchling-http

HTTP/1.1 retrieval with TLS, keep-alive pooling, cookies, and redirects for
building a network retriever (used by
[fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: an HTTP/1.1 client you wire into your own Tokio loop—download to a file (or
stdout when the destination is `-`), follow redirects, reuse keep-alive
connections, store cookies, upgrade via HSTS, talk through HTTP proxies, and
optionally capture WARC request/response bytes. Set `fetchling_core::Config`
fields directly; CLI / wget names in field docs are compatibility aliases.

IS NOT: FTP, HTTP/2, recursive mirroring, HTML/CSS extraction, or writing
`.warc` files. Those belong in other `fetchling-*` crates or in your own code.
This crate does not re-export `Config`, `Error`, or `Logger`.

The public API is flat at the crate root (`fetchling_http::HttpClient`,
`fetchling_http::Jar`, and so on). `download` needs a Tokio runtime. Transport,
TLS, proxies, and HSTS primitives come from `fetchling-net`.

## Typical integration

1. Start from `Config::default()` and set HTTP fields (`user_agent`, proxies,
   `cookies` / `load_cookies`, `hsts`, `continue_download`, `max_redirect`, TLS)
2. Create `Logger::new(&cfg)` then `HttpClient::new(&cfg, log)`
3. Call `download`
4. Optionally `save_cookies` / `save_hsts`
5. Optionally use `Jar` or the Content-Disposition / Content-Range helpers

```rust
use fetchling_http::{
    parse_content_disposition_filename, parse_content_range_total, resume_progress_total,
};

assert_eq!(
    parse_content_disposition_filename(r#"attachment; filename="report.pdf""#).as_deref(),
    Some("report.pdf")
);
assert_eq!(parse_content_range_total("bytes 0-499/1234"), Some(1234));
assert_eq!(
    resume_progress_total(100, Some(50), Some("bytes 100-149/500")),
    Some(500)
);
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
