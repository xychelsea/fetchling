//! Shared foundation types for building a network retriever.
//!
//! # What this crate is (and is not)
//!
//! IS: configuration ([`Config`], wgetrc/netrc), errors ([`Error`],
//! [`ExitCode`]), logging and progress ([`Logger`], [`ProgressBar`]), URL helpers
//! ([`FetchUrl`], [`normalize_url`]), character encoding, value parsers, and
//! simple glob matching. The public API is flat: items are re-exported at the
//! crate root. Set [`Config`] fields directly in library code; CLI / wget names
//! in field docs are compatibility aliases.
//!
//! IS NOT: HTTP/FTP retrieval, TLS, connection pooling, scheduling, or
//! recursive mirroring—those live in other `fetchling-*` crates or your own
//! transfer code.
//!
//! # Typical integration
//!
//! 1. Start from [`Config::default`] (optionally [`apply_wgetrc_command`] /
//!    [`load_wgetrc_files`], then [`Config::finalize_concurrency`])
//! 2. Normalize URLs ([`normalize_url`] / [`normalize_url_iri`])
//! 3. Resolve credentials ([`lookup_credentials`] / [`parse_netrc`]) when needed
//! 4. Create [`Logger`] and [`ProgressBar`] around your transfer loop
//! 5. Map failures with [`Error::exit_code`] and aggregate with [`ExitCode::worse`]
//!
//! # Areas
//!
//! - **Config** — [`Config`], [`apply_wgetrc_command`], [`load_wgetrc_files`],
//!   [`Netrc`], [`parse_netrc`], [`lookup_credentials`]
//! - **Errors** — [`Error`], [`ExitCode`], [`Result`]
//! - **Encoding** — [`resolve_encoding`], [`decode_bytes`], [`charset_from_content_type`]
//! - **Parse** — [`ByteSize`], [`parse_bytes`], [`parse_seconds`], [`parse_tries`]
//! - **Glob** — [`match_glob`]
//! - **URL** — [`FetchUrl`], [`normalize_url`], [`normalize_url_iri`], [`strip_query_vars`]
//! - **Progress** — [`Logger`], [`ProgressBar`], and related format helpers
//!
//! # Examples
//!
//! Config and URL normalization:
//!
//! ```
//! use fetchling_core::{normalize_url, Config};
//!
//! let mut cfg = Config::default();
//! cfg.max_threads = 4;
//! cfg.finalize_concurrency();
//! assert_eq!(cfg.max_threads_per_host, 4);
//!
//! let url = normalize_url("example.com/a").unwrap();
//! assert_eq!(url.scheme(), "http");
//! assert_eq!(url.host_str(), Some("example.com"));
//! ```
//!
//! wgetrc command and in-memory netrc:
//!
//! ```
//! use fetchling_core::{apply_wgetrc_command, parse_netrc, Config};
//!
//! let mut cfg = Config::default();
//! apply_wgetrc_command(&mut cfg, "quiet = on").unwrap();
//! assert!(cfg.quiet);
//!
//! let netrc = parse_netrc(
//!     "machine example.com\n  login alice\n  password secret\n",
//! )
//! .unwrap();
//! let entry = netrc.lookup("example.com").unwrap();
//! assert_eq!(entry.login.as_deref(), Some("alice"));
//! ```
//!
//! Progress bar lifecycle (quiet config avoids TTY output in tests):
//!
//! ```
//! use fetchling_core::{Config, ProgressBar};
//!
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! let mut bar = ProgressBar::new(&cfg, Some(100), "file.bin");
//! bar.update(40);
//! bar.update(60);
//! bar.finish();
//! ```

#![warn(missing_docs)]

mod config;
mod encoding;
mod error;
mod globutil;
mod parse;
mod progress;
mod url_util;

pub use config::{
    apply_wgetrc_command, load_wgetrc_files, lookup_credentials, parse_netrc, Config, Netrc,
    NetrcEntry,
};
pub use encoding::{charset_from_content_type, decode_bytes, resolve_encoding};
pub use error::{Error, ExitCode, Result};
pub use globutil::match_glob;
pub use parse::{parse_bytes, parse_seconds, parse_tries, ByteSize};
pub use progress::*;
pub use url_util::{normalize_url, normalize_url_iri, strip_query_vars, FetchUrl};
