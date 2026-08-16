# fetchling-cli

wget-compatible argv parsing into `Config` for building a network retriever
(used by [fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: a synchronous argv parser you call from your own binary—fill a
`fetchling_core::Config` from wget-style long/short flags, `-n` packs
(`-nc` / `-nd` / `-nH` / `-np` / `-nv`), `--` end-of-options, and URL
operands. Loads wgetrc unless `--no-config`. Rejects unimplemented flags.
Help/version printers write wget-style text to stdout. CLI / wget names on
`Config` fields are compatibility aliases.

IS NOT: job execution, an HTTP or FTP client, or HTML/CSS/Metalink/WARC
parsers. Those belong in other `fetchling-*` crates or in your own code. This
crate does not re-export `Config` or `Error`. It does not daemonize or install
a Tokio runtime; pass `*cfg` to your retriever (for example
`fetchling_engine::Engine`) yourself.

The public API is flat at the crate root (`fetchling_cli::parse_args`,
`fetchling_cli::ParseOutcome`, and so on).

## Typical integration

1. Call `parse_args` on process argv (leading `fetchling` / `./fetchling` is
   stripped; other program names are left as operands)
2. Match `ParseOutcome`: `Help` / `Version` / `VersionShort` → print helpers;
   `Run(cfg)` → `*cfg` into your retriever
3. Optionally `is_deferred_option` if you inspect flags yourself

```rust
use fetchling_cli::{parse_args, ParseOutcome};

let ParseOutcome::Run(cfg) =
    parse_args(["fetchling", "--no-config", "-q", "http://example.com"]).unwrap()
else {
    panic!("expected run");
};
assert!(cfg.quiet);
assert_eq!(cfg.urls, vec!["http://example.com"]);
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
