# fetchling-core

Shared foundation types for building a network retriever (used by
[fetchling](https://github.com/xychelsea/fetchling)): configuration, errors and
exit codes, logging and progress, URL helpers, encoding/parse utilities, and
`.netrc` / `.wgetrc` support.

## What this crate is (and is not)

IS: reusable plumbing you wire into your own download loop—set `Config`
fields, normalize URLs, resolve credentials, log status, draw a progress bar,
and map failures to process exit codes.

IS NOT: HTTP/FTP retrieval, TLS, connection pooling, scheduling, or
recursive mirroring. Those belong in other `fetchling-*` crates or in your own
code.

The public API is flat at the crate root (`fetchling_core::Config`,
`fetchling_core::Error`, and so on). Field docs often mention wget-compatible
CLI names as aliases; library users set the fields directly.

## Typical integration

1. Start from `Config::default()` (optionally apply wgetrc, then
   `finalize_concurrency`)
2. Normalize URLs with `normalize_url` / `normalize_url_iri`
3. Resolve credentials with `lookup_credentials` or `parse_netrc` when needed
4. Create `Logger` and `ProgressBar` around your transfer loop
5. Map failures with `Error::exit_code` and aggregate with `ExitCode::worse`

```rust
use fetchling_core::{
    apply_wgetrc_command, normalize_url, parse_netrc, Config, Logger,
};

let mut cfg = Config::default();
cfg.quiet = true;
apply_wgetrc_command(&mut cfg, "tries = 3").unwrap();
cfg.finalize_concurrency();

let url = normalize_url("example.com/file.bin").unwrap();
assert_eq!(url.scheme(), "http");

let netrc = parse_netrc("machine example.com\nlogin u\npassword p\n").unwrap();
assert_eq!(netrc.lookup("example.com").unwrap().login.as_deref(), Some("u"));

let log = Logger::new(&cfg).unwrap();
log.info(&format!("fetching {}", url.as_str()));
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
