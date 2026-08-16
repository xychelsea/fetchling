# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - 2026-08-16

Documentation Update: docs.rs-oriented crate documentation and broader tests. No intentional retrieval-behavior change.

### Added

- Project rustdoc and crate READMEs for `fetchling-core`, `fetchling-cli`, `fetchling-net`, `fetchling-http`, `fetchling-ftp`, `fetchling-formats`, and `fetchling-engine`
- `fetchling` library surface (`run` / `run_with_args`) plus docs.rs badge and library-crates notes in the workspace README
- Packed unit tests and localhost integration tests (gzip, headers/POST, `-nc`, accept/CSS recurse, retry, `-i`, binary `-O -`)

## [0.1.1] - 2026-08-11

Experimental follow-up release: security hardening, download concurrency/performance, and local benchmarking helpers.

### Added

- `--max-threads-per-host` to cap concurrent connections per host (default `min(max-threads, 4)`)
- `SECURITY.md` with supported versions and vulnerability reporting guidance
- `scripts/wget-benchmark.sh` and README benchmark results for local wget comparisons

### Changed

- Default per-host concurrency raised from a hard cap of 2 to `min(max-threads, 4)`
- Buffered HTTP body writes with `block_in_place` around flushes for better async throughput
- TCP_NODELAY on connections; idle connection-pool sizing tied to configured concurrency

### Security

- `robots.txt` fails closed when fetch or parse fails
- FTP symlink targets hardened to basename-only local names
- Warning when `.netrc` permissions are too open
- Clearer warnings for password-bearing URLs and `--no-check-certificate`

### Fixed

- Askpass, progress meter, and daemon path edge cases
- `cookie_store` dependency bump

### Not in this release

- crates.io publish (install from GitHub Releases or source)
- Cross-platform release binaries (Linux x86_64 only for the GitHub Release artifact)
- Full wget feature parity (see Feature status in the README)

## [0.1.0] - 2026-08-10

First public **experimental** release. `fetchling` is not a drop-in replacement for `wget`.

### Added

- HTTP/1.1 and HTTPS (rustls) downloads with redirects, resume (`-c`), timestamping (`-N`), cookies, Basic auth, and custom methods/bodies
- Recursive HTML/CSS retrieval, page requisites, mirroring helpers, and `robots.txt` enforcement (including non-default ports)
- FTP/FTPS passive and active transfers, listings, and globbing
- Metalink and WARC support (partial; see Feature status in the README)
- HTTP(S) proxy support (absolute-form / CONNECT)
- Localhost behavior matrix tests and a CI `wget` comparison script for core download cases
- Warnings for accepted-but-inert settings (`wgetrc robots=`, `ftp_proxy`, `--ciphers`, RNG seed files)

### Not in this release

- crates.io publish (workspace crates remain path-only; install from GitHub Releases or source)
- Full wget feature parity (Digest/NTLM auth, SOCKS/FTP proxies, EPSV, HTTP/2, Metalink segmentation, and other Feature status gaps)
- Cross-platform release binaries (Linux x86_64 only for the GitHub Release artifact)

[0.1.2]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.2
[0.1.1]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.1
[0.1.0]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.0
