# fetchling

[![CI](https://github.com/xychelsea/fetchling/actions/workflows/ci.yml/badge.svg)](https://github.com/xychelsea/fetchling/actions/workflows/ci.yml)
[![docs.rs](https://docs.rs/fetchling/badge.svg)](https://docs.rs/fetchling)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

`fetchling` is a modular, non-interactive network retriever written in Rust for downloading files and recursively mirroring remote content from the command line.

`fetchling` is built on an asynchronous networking stack built around Tokio, Hyper, and rustls. The project separates command-line parsing, protocol handling, recursive retrieval, network primitives, and format processing into independent workspace crates.

> [!IMPORTANT]
> `fetchling` is under active development. Core http/https downloading and recursive retrieval are implemented, but some command-line options are incomplete or not yet applied and tested. `fetchling` is not currently compatible as a drop-in replacement for CLI downloader tools such as `wget`.

## Features

Fetchling currently includes:

- http/https downloads over http/1.1
- http redirects and persistent connection reuse
- resumable http downloads using range requests
- timestamp-based retrieval with `If-Modified-Since` and `Last-Modified`
- `gzip` http response decompression
- http cookies with load/save support
- custom request headers, methods, and request bodies
- output to files or standard output
- configurable retry, timeout, rate, and quota controls
- concurrent retrieval with global and per-host limits (`--max-threads`, `--max-threads-per-host`)
- ipv4 and ipv6 connection handling
- custom DNS servers and DNS bind address
- recursive html and css link discovery
- recursion depth, host, path, and accept/reject controls
- `robots.txt` handling during recursive retrieval
- conversion of downloaded links to local paths
- passive and active `ftp` downloads
- metalink input with multi-hash verification and mirror failover
- WARC request/response capture (IP address, Concurrent-To, payload digests)
- WARC gzip compression, sha-1 digests, size-based rotation, CDX, and dedup revisits
- public-key pinning (`--pinnedpubkey`)
- Unix background mode (`-b`)
- `wget`-style command-line and configuration conventions

The complete option list is available with:

```console
fetchling --help
```

## Quick start

### GitHub Release (experimental)

Download a prebuilt Linux x86_64 binary from the [Releases](https://github.com/xychelsea/fetchling/releases) page, verify the checksum, and place `fetchling` on your `PATH`.

### Build from source

`fetchling` requires Rust 1.75 or later.

```console
git clone https://github.com/xychelsea/fetchling.git
cd fetchling
cargo build --release -p fetchling
```

The binary will be written to:

```text
target/release/fetchling
```

Run it directly:

```console
./target/release/fetchling https://example.com/file.bin
```

Or install from a checkout or git:

```console
cargo install --path crates/fetchling
# or
cargo install --git https://github.com/xychelsea/fetchling fetchling
```

`fetchling` is published to [crates.io](https://crates.io/crates/fetchling).

### NixOS

A development build can also be produced with a temporary Nix shell (my environment):

```console
nix-shell -p cargo cargo-deny clippy rustc -I nixpkgs=channel:nixos-unstable \
  --run 'cargo build --release -p fetchling'
```

## Usage

```text
fetchling [OPTION]... [URL]...
```

Download a file:

```console
fetchling https://example.com/file.bin
```

Write a response to standard output:

```console
fetchling -O - https://example.com/robots.txt
```

Save a response under a specific name:

```console
fetchling -O archive.tar.zst https://example.com/releases/latest.tar.zst
```

Resume a partially downloaded file:

```console
fetchling -c https://example.com/large-image.iso
```

Only retrieve a file when the remote resource is newer:

```console
fetchling -N https://example.com/archive.tar.gz
```

Read URLs from a file:

```console
fetchling -i urls.txt
```

Use several concurrent retrieval jobs:

```console
fetchling --max-threads 8 \
  https://example.com/a.bin \
  https://example.net/b.bin \
  https://example.org/c.bin
```

`--max-threads` accepts values from 1 through 32 and defaults to 1. A separate `--max-threads-per-host` limit caps simultaneous transfers to one host (1..=32). When unset, it defaults to `min(max-threads, 4)` so a global job pool does not open the same number of connections to a single host unless you raise the per-host cap explicitly:

```console
fetchling --max-threads 8 --max-threads-per-host 8 -i urls.txt
```

## Recursive retrieval

Use `-r` or `--recursive` to retrieve a document and follow links discovered in downloaded HTML and CSS:

```console
fetchling -r https://example.com/
```

Limit recursion depth with `-l`:

```console
fetchling -r -l 2 https://example.com/
```

A recursive job broadly follows this process:

1. fetch the starting url
2. inspect downloaded html or css content
3. extract referenced urls
4. apply recursion and filtering rules
5. skip urls already visited during the current run
6. queue eligible URLs for retrieval
7. repeat until the queue or configured recursion depth is exhausted

Recursive and page-requisite retrieval consults the target host's `robots.txt`. `fetchling` caches the parsed rules for the duration of the run and avoids issuing duplicate simultaneous `robots.txt` requests for the same host.

### Page requisites

Retrieve resources needed by an html page with:

```console
fetchling -p https://example.com/page.html
```

`--page-requisites` follows discovered resources for one level even without a general recursive crawl.

### Mirroring

The familiar `wget`-style mirror shortcut is available:

```console
fetchling -m https://example.com/
```

`-m` enables recursive retrieval, timestamping, unlimited recursion depth, and preservation of ftp listing files.

For a locally browsable HTTP mirror, link conversion can be enabled separately:

```console
fetchling -r -k https://example.com/
```

Use `-K` to retain the original files before conversion:

```console
fetchling -r -k -K https://example.com/
```

### Controlling crawl scope

Prevent recursive retrieval from ascending above the starting path:

```console
fetchling -r --no-parent https://example.com/docs/project/
```

Allow links to other hosts:

```console
fetchling -r --span-hosts https://example.com/
```

Restrict recursive traversal to HTTPS URLs:

```console
fetchling -r --https-only https://example.com/
```

Follow only relative links:

```console
fetchling -r --relative https://example.com/
```

### Accept and reject filters

Accept selected file extensions:

```console
fetchling -r -A html,css,png,jpg https://example.com/
```

Reject selected extensions:

```console
fetchling -r -R zip,mp4,iso https://example.com/
```

URL regular expressions are also available:

```console
fetchling -r --accept-regex '/docs/' https://example.com/
```

or:

```console
fetchling -r --reject-regex '/archive/' https://example.com/
```

Recursive traversal can also be constrained by domain or directory:

```console
fetchling -r -D example.com,static.example.com https://example.com/
```

```console
fetchling -r -I /docs,/assets https://example.com/
```

See `fetchling --help` for the complete recursive accept/reject option set.

## http and https

`fetchling`'s http client is built directly on Hyper and currently uses http/1.1.

### Redirects

http redirects are followed automatically up to the configured limit:

```console
fetchling --max-redirect 10 https://example.com/resource
```

### Resume

`-c` / `--continue` resumes an existing partial http download by sending a `Range` request based on the current local file size:

```console
fetchling -c https://example.com/large-file.bin
```

If the server ignores the range request and returns the complete resource, `fetchling` restarts the download rather than appending a full response to the partial file.

An explicit byte offset can be supplied with:

```console
fetchling --start-pos 1048576 https://example.com/file.bin
```

### Timestamping

`-N` / `--timestamping` uses local and remote modification times to avoid unnecessary downloads:

```console
fetchling -N https://example.com/current.tar.gz
```

`fetchling` can send `If-Modified-Since` based on the existing local file and handles `304 Not Modified` responses. Server-provided `Last-Modified` timestamps can also be applied to downloaded files.

### Cookies

Cookies are enabled by default.

Load cookies before a session:

```console
fetchling --load-cookies cookies.txt https://example.com/
```

Save cookies after the session:

```console
fetchling --save-cookies cookies.txt https://example.com/
```

Session cookies can be retained when saving with:

```console
fetchling --keep-session-cookies \
  --save-cookies cookies.txt \
  https://example.com/
```

Disable cookies entirely with:

```console
fetchling --no-cookies https://example.com/
```

### Custom requests

Add an HTTP header:

```console
fetchling --header='Accept: application/json' https://example.com/api
```

Set a user agent:

```console
fetchling -U 'fetchling-example/1.0' https://example.com/
```

Send form-style POST data:

```console
fetchling --post-data='name=value' https://example.com/submit
```

Use an explicit method and body:

```console
fetchling \
  --method PUT \
  --body-data '{"enabled":true}' \
  --header='Content-Type: application/json' \
  https://example.com/api/item
```

### Compression

`fetchling` can request and decode gzip-encoded http responses:

```console
fetchling --compression=gzip https://example.com/file
```

The available modes are `auto`, `gzip`, and `none`.

### TLS

HTTPS connections use rustls.

Certificate verification is enabled by default. It can be disabled with:

```console
fetchling --no-check-certificate https://example.com/
```

Disabling certificate verification removes an important authentication check and should be used only when the consequences are understood.

Some of the exposed TLS compatibility options are not yet implemented; see [Known limitations](#known-limitations).

## FTP

`fetchling` implements basic ftp file retrieval in passive mode.

```console
fetchling ftp://ftp.example.com/pub/file.tar.gz
```

Authentication can be supplied with:

```console
fetchling \
  --ftp-user USER \
  --ftp-password PASS \
  ftp://ftp.example.com/private/file.bin
```

Anonymous FTP is used when credentials are not supplied.

FTP supports passive and active (`--no-passive-ftp`) retrieval. Directory URLs (trailing ending in `/`) fetch a `.listing` via `LIST`/`NLST` (kept when `--no-remove-listing` is set). `ftps://` uses explicit AUTH TLS by default; `--ftps-implicit` selects implicit FTPS (default port 990).

## Metalink

`fetchling` can read a local Metalink document:

```console
fetchling --input-metalink downloads.meta4
```

The current implementation selects a URL with matching `--preferred-location` when present; otherwise the highest `preference` value (then the first URL). When hashes are present, `fetchling` verifies md5, sha-1, sha-256, and/or sha-512 digests.

A checksum mismatch removes the downloaded file by default. To retain mismatched files for inspection:

```console
fetchling --keep-badhash --input-metalink downloads.meta4
```

`--metalink-over-http` recognizes Metalink `Content-Type` bodies and RFC 6249 `Link` headers (`rel=describedby` / `rel=duplicate`). `--metalink-index` selects a `<metaurl>` ordinal (1-based; `0`/`inf` uses file entries directly). Multi-source segmented downloading is not implemented.

## WARC

`fetchling` can record HTTP requests and responses in WARC 1.0 files while downloading content.

```console
fetchling \
  --warc-file capture.warc.gz \
  https://example.com/
```

WARC capture can also be combined with recursive retrieval:

```console
fetchling \
  -r \
  --warc-file mirror.warc.gz \
  https://example.com/
```

The current WARC writer supports:

- `warcinfo` records
- HTTP request records
- HTTP response records
- `WARC-IP-Address` from the peer address on request/response records
- optional gzip compression
- optional SHA-1 block and payload digests
- additional `warcinfo` headers
- size-based WARC file rotation
- `--warc-cdx` sidecar index files
- `--warc-dedup` revisit records (identical-payload-digest profile) against a prior CDX, with CDX lines still written
- embedding the session logfile as a WARC `resource` when `--warc-keep-log` is enabled (default) and a logfile is configured

Add information to the `warcinfo` record with:

```console
fetchling \
  --warc-file capture.warc.gz \
  --warc-header 'operator: example' \
  https://example.com/
```

Disable WARC compression with:

```console
fetchling \
  --warc-file capture.warc \
  --no-warc-compression \
  https://example.com/
```

Set a maximum segment size with:

```console
fetchling \
  --warc-file capture.warc.gz \
  --warc-max-size 1G \
  https://example.com/
```

Without `--warc-tempdir`, response bodies larger than 64 MiB are omitted from the WARC record (headers only) with a warning; with `--warc-tempdir`, bodies spill to disk without that cap.

## Input and output control

### Standard input

A URL list can be read from standard input:

```console
printf '%s\n' \
  https://example.com/a \
  https://example.com/b |
  fetchling -i -
```

### HTML input

An input file can be treated as HTML and scanned for URLs:

```console
fetchling -F -i links.html
```

Use a base URL when resolving relative references:

```console
fetchling \
  -F \
  -B https://example.com/docs/ \
  -i links.html
```

### Output directory

Place retrieved files under a prefix:

```console
fetchling -P downloads https://example.com/file.bin
```

Recursive downloads can preserve host and path structure according to the directory-related options documented by `fetchling --help`.

### Existing files

Skip an existing destination:

```console
fetchling --no-clobber https://example.com/file.bin
```

Use numbered names on collisions:

```console
fetchling --unique-names https://example.com/file.bin
```

Rotate numbered backups before overwriting:

```console
fetchling --backups 3 https://example.com/file.bin
```

## Transfer controls

Limit the aggregate retrieval quota:

```console
fetchling -Q 1G https://example.com/large-file.bin
```

Limit the download rate:

```console
fetchling --limit-rate 2M https://example.com/file.bin
```

Set a general timeout:

```console
fetchling -T 30 https://example.com/
```

Individual DNS, connect, and read timeouts can also be configured:

```console
fetchling \
  --dns-timeout 5 \
  --connect-timeout 10 \
  --read-timeout 30 \
  https://example.com/
```

Force IPv4:

```console
fetchling -4 https://example.com/
```

Force IPv6:

```console
fetchling -6 https://example.com/
```

fetchling also provides separate retry, retry-delay, inter-request wait, and randomized wait controls. See `fetchling --help` for their current syntax.

## Feature status

`fetchling` is still at an extremely early stage. This table describes the current implementation rather than the full set of options accepted by the parser. Working features are listed under [Features](#features). Parser-rejected options are summarized under Deferred; the canonical names live in `crates/fetchling-cli/src/deferred.rs`.

| Capability | Status | Notes |
| --- | --- | --- |
| HTTP authentication | Partial | Basic only; no 401 challenge / Digest / NTLM |
| Proxy routing | Partial | HTTP absolute-form + HTTPS CONNECT; env + `NO_PROXY`; no SOCKS |
| Passive FTP (EPSV) | Partial | PASV + REST; no EPSV yet |
| Metalink | Partial | Preference, hashes, Link/`--metalink-index`, mirror failover; no multi-source segmented download |
| WARC | Partial | HTTP capture, digests, CDX, dedup, rotation; large bodies need `--warc-tempdir`; not FTP |
| Background / daemon (`-b`) | Partial | Unix daemonize; non-Unix warns and stays foreground |
| FTP proxies | Not implemented | `ftp_proxy` / `FTP_PROXY` warned and ignored; FTP client never proxies |
| wgetrc `robots=` | Not implemented | Warned no-op; robots always enforced when recursing |
| Custom TLS ciphers | Deferred | `--ciphers` rejected until rustls cipher configuration exists |
| `--random-file` / `--egd-file` | Not implemented | Accepted, ignored with warning |
| HTTP/2 | Deferred | Hyper HTTP/1 only; `--http2`, `--http2-only`, `--http2-request-window` |
| Chunked / parallel download | Deferred | `--chunk-size` |
| wget2 Metalink flag | Deferred | `--metalink` (distinct from implemented `--input-metalink` / `--force-metalink`) |
| OCSP | Deferred | `--ocsp*` family |
| HPKP / HSTS preload | Deferred | `--hpkp*`, `--hsts-preload*` |
| TLS session extras | Deferred | `--dane`, `--tls-false-start`, `--tls-resume`, `--tls-session-file`, `--tcp-fastopen` |
| HTTPS policy extras | Deferred | `--https-enforce`, `--check-hostname` |
| Plugins | Deferred | `--plugin*`, `--list-plugins`, `--local-plugin` |
| Stats sinks | Deferred | `--stats-*` |
| Signature verification | Deferred | `--verify-sig*`, `--gnupg-homedir`, `--signature-extensions` |
| Other wget2 CLI | Deferred | Remaining names in `deferred.rs` (`bind-interface`, `cookie-suffixes`, `local-db`, `download-attr`, …) |

## Architecture

Fetchling is organized as a `cargo` workspace with eight crates:

| Crate | Responsibility |
| --- | --- |
| [`fetchling`](https://docs.rs/fetchling) | Executable entry point: argv, rustls, daemonize, Tokio, and engine |
| [`fetchling-cli`](https://docs.rs/fetchling-cli) | Command-line metadata, parsing, help, and option handling |
| [`fetchling-core`](https://docs.rs/fetchling-core) | Shared configuration, errors, logging, progress, and URL utilities |
| [`fetchling-net`](https://docs.rs/fetchling-net) | DNS, TCP, TLS, HTTP-proxy, and rate-limiting primitives |
| [`fetchling-http`](https://docs.rs/fetchling-http) | HTTP/1.1 retrieval with TLS, keep-alive pooling, cookies, and redirects |
| [`fetchling-ftp`](https://docs.rs/fetchling-ftp) | FTP/FTPS retrieval, listing, and glob expansion |
| [`fetchling-formats`](https://docs.rs/fetchling-formats) | Metalink, WARC, robots.txt, and HTML/CSS/feed link extraction |
| [`fetchling-engine`](https://docs.rs/fetchling-engine) | Recursive HTTP/FTP retrieval orchestration with robots, metalink, and path policy |

### Library crates

The `fetchling` crate is the command-line binary (and a thin `run` / `run_with_args` wrapper). Protocol and orchestration crates are documented for independent use: depend on them directly, set `Config` fields in library code, and do not expect them to re-export `Config` or `Error`.

- [`fetchling-cli`](https://docs.rs/fetchling-cli) — argv parsing into `Config`
- [`fetchling-core`](https://docs.rs/fetchling-core) — `Config`, errors, logging, progress, URLs
- [`fetchling-net`](https://docs.rs/fetchling-net) — DNS, TCP, TLS, proxies, rate limits
- [`fetchling-http`](https://docs.rs/fetchling-http) — HTTP/1.1 client
- [`fetchling-ftp`](https://docs.rs/fetchling-ftp) — FTP/FTPS client
- [`fetchling-formats`](https://docs.rs/fetchling-formats) — Metalink, WARC, robots, link extraction
- [`fetchling-engine`](https://docs.rs/fetchling-engine) — recursive retrieval job runner

The engine owns the retrieval queue and shared crawl state. Independent jobs are run through Tokio tasks subject to a global semaphore controlled by `--max-threads`. A separate per-host semaphore (`--max-threads-per-host`, default `min(max-threads, 4)`) limits simultaneous retrievals from a single host. The engine also keeps shared state for:

- visited URL deduplication
- recursive work queues
- `robots.txt` caching
- destination-path locks
- metalink hashes
- warc output
- retrieval quota accounting
- aggregate exit status

Destination locks prevent concurrent jobs from writing the same local path at the same time.

## Known limitations!

See [Feature status](#feature-status) for incomplete and deferred tracking. The notes below cover operational detail for areas that are already usable.

### http 1.1

http and https can use proxies via `--http-proxy` / `--https-proxy` / `--proxy` or `http_proxy` / `https_proxy` environment variables, with `--proxy-user` / `--proxy-password` (or userinfo) and `NO_PROXY` / `no_proxy` bypass. Plain http uses absolute-form requests; https uses CONNECT.

### TLS certificates

`--crl-file`, `--certificate` / `--private-key` (PEM/DER/ASN1), `--ca-directory`, `--secure-protocol` (`auto` / `TLSv1_2` / `TLSv1_3`), and `--pinnedpubkey` are implemented. Legacy values such as `SSLv3` / `TLSv1` / `PFS` are rejected.

Other TLS compatibility options should be considered experimental until their behavior has been tested more extensively.

### ftp/ftps

- ftp supports passive retrieval and active mode (`--no-passive-ftp` with PORT/EPRT).
- Directory URLs write `.listing` (deleted by default; keep with `--no-remove-listing`).
- ftp globbing (`*` `?` `[…]`) expands via `NLST`/`LIST` when `--no-glob` is not set.
- `--preserve-permissions` applies Unix modes from MLST `unix.mode` when available, otherwise from Unix `LIST` permission bits.
- `ftps://` uses explicit AUTH TLS (port 21 by default). `--ftps-implicit` starts TLS immediately (port 990 when unset/21). `--ftps-clear-data-connection` uses `PROT C`; otherwise data uses `PROT P`. `--no-ftps-resume-ssl` disables TLS session resumption on the data channel. `--ftps-fallback-to-ftp` continues as plain FTP if `AUTH TLS` is rejected.
- `--retr-symlinks` retrieves FTP symlink targets as files; without it, local symlinks are created when possible.

### Metalink

- `fetchling` selects mirrors by `--preferred-location` then preference, and fails over to later mirrors on transfer or hash failure.
- `--metalink-over-http` and `--force-metalink` handle Metalink bodies; Link headers work with `--metalink-over-http`; `--metalink-index` selects a metaurl ordinal.
- md5 / sha-1 / sha-256 / sha-512 verification is supported when the Metalink entry provides hashes.

### Build

Build the full workspace:

```console
cargo build --workspace
```

Build an optimized Fetchling binary:

```console
cargo build --release -p fetchling
```

Run directly through Cargo:

```console
cargo run -p fetchling -- https://example.com/
```

Pass command-line options after `--`:

```console
cargo run -p fetchling -- -r -l 1 https://example.com/
```

### Format

CI requires the workspace to pass rustfmt:

```console
cargo fmt --all -- --check
```

To apply formatting:

```console
cargo fmt --all
```

### Linter

Run Clippy with warnings treated as errors:

```console
cargo clippy --workspace --all-targets -- -D warnings
```

### Docs

Build workspace rustdoc with missing-docs warnings treated as errors:

```console
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

### Test

Run all workspace tests:

```console
cargo test --workspace
```

### Benchmark

Compare localhost wall time and throughput against `wget` with:

```console
./scripts/wget-benchmark.sh
```

Or, in NixOS (my environment):

```console
nix-shell -p cargo rustc wget python3 -I nixpkgs=channel:nixos-unstable \
  --run 'cargo build --release -p fetchling && ./scripts/wget-benchmark.sh'
```

The harness serves temporary fixtures on `127.0.0.1`, times quiet downloads for a small file, a large file, and a multi-URL batch (`fetchling --max-threads 8 --max-threads-per-host 4` vs sequential `wget`), and checks downloaded bytes with `cmp`. Optional knobs (`LARGE_MIB`, `MULTI_PARTS`, `SMALL_RUNS`, `PER_HOST_THREADS`, and others) are documented at the top of the script.

Sample results on Linux x86_64 against GNU Wget 1.25.0 (localhost HTTP only; not WAN-representative), using a `release` `fetchling` binary (`cargo build --release -p fetchling`).

| Scenario | fetchling | wget | ratio (fl/wg) |
| --- | ---: | ---: | ---: |
| Small (1 KiB, mean of 20) | 4.33 ms | 4.37 ms | 0.99 |
| Large (256 MiB, mean of 3) | 0.410 s (624 MiB/s) | 0.470 s (544 MiB/s) | 0.87 |
| Multi (16×4 MiB, mean of 3) | 0.057 s (1123 MiB/s, `--max-threads 8 --max-threads-per-host 4`) | 0.201 s (318 MiB/s, sequential) | 0.28 |

Integrity checks passed for every scenario. The multi-URL case is not an apples-to-apples concurrency comparison: `wget` was timed as a sequential loop because it has no native job pool comparable to `--max-threads`.

### Dependency policy

The repository uses `cargo-deny` for dependency checks:

```console
cargo deny check --all-features
```

The GitHub Actions workflow runs formatting, Clippy, workspace tests, and `cargo-deny` on pushes and pull requests to `main`.

## Contributing

`fetchling` is under active development. Bug reports, implementation fixes, tests, documentation improvements, and focused protocol work are welcome.

Before submitting a pull request, run:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check --all-features
./scripts/wget-compare.sh
```
When implementing an existing command-line option, prefer making its behavior explicit and testable. Options that are accepted but not implemented should fail clearly or emit a clear warning rather than silently implying behavior that does not occur.

For compatibility changes, include tests for the specific retrieval behavior rather than relying only on command-line parsing tests.

## Project status

`fetchling` **0.1.1** is an experimental public release. The advertised [Features](#features) surface is covered by localhost behavior tests and a short CI `wget` comparison; remaining gaps are tracked under [Feature status](#feature-status).

Release gate for this version:

- behavior matrix + `scripts/wget-compare.sh` green in CI
- accepted-but-inert options warn or reject clearly (`robots=` wgetrc, `ftp_proxy`, deferred flags)

Post-`0.1.1` priorities remain reliability and selective parity work (not full wget2 coverage):

- proxy transport (SOCKS / FTP proxy)
- HTTP auth challenges (Digest / NTLM)
- FTP completion (EPSV)
- TLS option hardening
- crates.io publishing (versioned workspace deps)
- stabilization of crate APIs

The issue tracker should be used for concrete feature requests and implementation work.

## License

`fetchling` is a Rust project with a permissive license under either of:

- [Apache License, Version 2.0](LICENSE)
- [MIT License](LICENSE-MIT)

