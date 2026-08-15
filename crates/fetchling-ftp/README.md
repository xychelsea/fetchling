# fetchling-ftp

FTP/FTPS retrieval, listing, and glob expansion primitives for building a
network retriever (used by [fetchling](https://github.com/xychelsea/fetchling)).

## What this crate is (and is not)

IS: an FTP/FTPS client you wire into your own Tokio loop—download files, write
directory listings, expand globs, and parse `MLSD` / Unix `LIST` text. Set
`fetchling_core::Config` fields directly; CLI / wget names in field docs are
compatibility aliases.

IS NOT: HTTP, connection pooling, recursive mirroring, symlink enqueue policy,
or SOCKS/HTTP proxies. Recursion and `.listing` destination naming belong in
other `fetchling-*` crates or in your own code. This crate does not re-export
`Config` or `Error`.

The public API is flat at the crate root (`fetchling_ftp::FtpClient`,
`fetchling_ftp::parse_mlsd`, and so on). Async entry points need a Tokio
runtime. Transport and TLS come from `fetchling-net`.

## Typical integration

1. Start from `Config::default()` and set FTP/FTPS fields (`ftp_user` /
   `ftp_password`, `passive_ftp`, `ftps_*`, `continue_download`,
   `preserve_permissions`)
2. Create `FtpClient::default()` (optionally reuse or replace `client.dns`)
3. Call `download` for a file URL or a directory URL (path ending in `/`)
4. Call `expand_glob` when the last path segment contains `*`, `?`, or `[…]`
5. Optionally parse listing text with `parse_mlsd` / `parse_unix_list`

```rust
use fetchling_ftp::{parse_mlsd, parse_unix_list, parse_unix_mode_from_list_line, FtpEntryKind};

let entries = parse_mlsd("type=file;size=1; readme.txt\ntype=dir; docs\n");
assert_eq!(entries[0].name, "readme.txt");
assert_eq!(entries[0].kind, FtpEntryKind::File);

let entries = parse_unix_list(
    "-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt\n\
     drwxr-xr-x 2 user group 4096 Jan  1 12:00 docs\n",
);
assert_eq!(entries[1].kind, FtpEntryKind::Dir);

assert_eq!(
    parse_unix_mode_from_list_line("-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt"),
    Some(0o644)
);
```

See the [workspace README](https://github.com/xychelsea/fetchling) for the full
fetchling product.
