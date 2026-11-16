# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Maintenance: dependency refresh and toolchain fixes. No intentional retrieval-behavior change.

### Changed

- Minimum supported Rust version is now declared and enforced as **1.88**. The previous `1.75` claim was never inherited by the crates and was already unbuildable with the locked dependencies.
- Workspace resolver `3` (MSRV-aware dependency resolution)
- Semver-compatible lockfile refresh (tokio 1.54, rustls 0.23.46, hyper 1.12, hickory 0.26.3, and others)
- Dependency upgrades: `webpki-roots` 1, `sha1`/`sha2`/`md-5` 0.11, `base64` 0.23, `quick-xml` 0.42, `nix` 0.31
- `tokio-rustls` no longer enables its default `aws-lc-rs` backend; the build now compiles only the `ring` provider fetchling actually uses
- `nix`, `posix-regex`, and `x509-parser` versions are now managed in `[workspace.dependencies]`
- CI: `actions/checkout` v7, `softprops/action-gh-release` v3, new MSRV and rustdoc jobs, weekly scheduled run; Dependabot for Cargo and GitHub Actions

### Fixed

- `clippy::needless_bool` failure on Rust 1.100 that broke CI
- Unused `hex` dependency in `fetchling-net`; redundant `homepage` manifest fields
- README `cargo deny` invocation for current cargo-deny (`cargo deny --all-features check`)

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

[Unreleased]: https://github.com/xychelsea/fetchling/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.2
[0.1.1]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.1
[0.1.0]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.0
