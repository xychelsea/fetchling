# fetchling-net

DNS, TCP, TLS, HTTP-proxy, and rate-limiting primitives for building a network
retriever (used by [fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: reusable transport plumbing you wire into your own Tokio client—resolve
hosts, open TCP (including a Happy Eyeballs race), build a rustls connector,
talk to HTTP proxies, limit download rate, and remember HSTS upgrades. Set
`fetchling_core::Config` fields directly; CLI / wget names in field docs are
compatibility aliases.

IS NOT: HTTP request/response framing, FTP, connection pooling, recursive
retrieval, or SOCKS. Those belong in other `fetchling-*` crates or in your own
protocol code. This crate does not re-export `Config` or `Error`.

The public API is flat at the crate root (`fetchling_net::DnsCache`,
`fetchling_net::build_connector`, and so on). Async entry points need a Tokio
runtime.

## Typical integration

1. Start from `Config::default()` and set net fields (timeouts, DNS, TLS, proxies,
   `limit_rate`)
2. Create `DnsCache::new()` and call `lookup`
3. Connect with `connect_happy_eyeballs` / `connect_tcp`, or go through a proxy
   (`proxy_url_for` → `proxy_bypassed` → `connect_to_proxy` or
   `connect_via_http_connect`)
4. Call `build_connector` and handshake with the returned
   `tokio_rustls::TlsConnector`
5. Wrap body reads with `RateLimiter::new(cfg.limit_rate)`; optionally use
   `HstsStore` for HTTP→HTTPS upgrades

```rust
use fetchling_core::Config;
use fetchling_net::{
    format_http_connect_request, host_matches_no_proxy, proxy_endpoint_key,
    RateLimiter,
};

let mut cfg = Config::default();
cfg.limit_rate = Some(64 * 1024);

assert!(RateLimiter::new(cfg.limit_rate).is_some());
assert_eq!(
    proxy_endpoint_key(Some("http://proxy.example:8080/")),
    "http://proxy.example:8080"
);
assert!(host_matches_no_proxy("a.example.com", "example.com"));

let req = format_http_connect_request("example.com", 443, None);
assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
