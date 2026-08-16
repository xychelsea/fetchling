# fetchling-engine

Recursive HTTP/FTP retrieval orchestration with robots, metalink, and
destination path policy for building a network retriever (used by
[fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: an async job runner you wire into your own Tokio loop—fill a
`fetchling_core::Config`, queue URLs, retrieve over HTTP/HTTPS and FTP/FTPS,
apply concurrency limits, follow robots/sitemaps, extract links for recursion,
handle Metalink and WARC, and save cookies/HSTS. Destination helpers map URLs
to local paths and apply clobber/backup policy. Set `Config` fields directly;
CLI / wget names in field docs are compatibility aliases. `Engine::run` needs
a Tokio runtime.

IS NOT: CLI/argv parsing, an HTTP or FTP client implementation, or HTML/CSS/
Metalink/WARC parsers. Those belong in other `fetchling-*` crates or in your
own code. This crate does not re-export `Config`, `Error`, `Logger`,
`ExitCode`, `HttpClient`, or `FtpClient`.

The public API is flat at the crate root (`fetchling_engine::Engine`,
`fetchling_engine::local_path_for_url`, and so on).

## Typical integration

1. Start from `Config::default()` and set `urls` plus retrieval fields
   (`recursive`, `directory_prefix`, proxies, TLS, `continue_download`, …)
2. Create `Engine::new(cfg)`
3. Call `run().await`
4. Optionally use destination helpers without constructing `Engine`

```rust
use std::path::PathBuf;
use fetchling_core::Config;
use fetchling_engine::local_path_for_url;
use url::Url;

let cfg = Config::default();
let url = Url::parse("http://example.com/a/b.txt").unwrap();
assert_eq!(local_path_for_url(&cfg, &url), PathBuf::from("./b.txt"));
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
