# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.0]: https://github.com/xychelsea/fetchling/releases/tag/v0.1.0
