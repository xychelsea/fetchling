//! wget-compatible argv parsing into [`Config`] for building a network retriever.
//!
//! # What this crate is (and is not)
//!
//! IS: a synchronous argv parser ([`parse_args`]) that fills a [`Config`] from
//! wget-style long/short flags, `-n` packs (`-nc` / `-nd` / `-nH` / `-np` /
//! `-nv`), `--` end-of-options, and URL operands. Loads wgetrc unless
//! `--no-config`. Rejects unimplemented flags via [`is_deferred_option`]
//! ([`Error::DeferredOption`]). Help/version printers ([`print_help`],
//! [`print_version`], [`print_version_short`]). The public API is flat at the
//! crate root. CLI / wget names on [`Config`] fields are compatibility aliases.
//!
//! IS NOT: job execution (`fetchling-engine`), an HTTP or FTP client, or
//! HTML/CSS/Metalink/WARC parsers. This crate does not re-export [`Config`] or
//! [`Error`]. It does not daemonize or install a Tokio runtime (that lives in
//! the caller / `fetchling` binary).
//!
//! # Typical integration
//!
//! 1. Call [`parse_args`] on process argv (leading `fetchling` / `./fetchling`
//!    is stripped; other program names are left as operands)
//! 2. Match [`ParseOutcome`]: [`ParseOutcome::Help`] / [`ParseOutcome::Version`]
//!    / [`ParseOutcome::VersionShort`] → print helpers; [`ParseOutcome::Run`] →
//!    `*cfg` into your retriever (e.g. `fetchling_engine::Engine::new`)
//! 3. Optionally [`is_deferred_option`] if you inspect flags yourself
//!
//! # Areas
//!
//! - **Parse** — [`parse_args`]
//! - **Outcome** — [`ParseOutcome`]
//! - **Deferred** — [`is_deferred_option`]
//! - **Help** — [`print_help`], [`print_version`], [`print_version_short`]
//!
//! # Examples
//!
//! Parse quiet download flags (no wgetrc):
//!
//! ```
//! use fetchling_cli::{parse_args, ParseOutcome};
//!
//! let ParseOutcome::Run(cfg) =
//!     parse_args(["fetchling", "--no-config", "-q", "http://example.com"]).unwrap()
//! else {
//!     panic!("expected run");
//! };
//! assert!(cfg.quiet);
//! assert_eq!(cfg.urls, vec!["http://example.com"]);
//! ```
//!
//! `--help` is a distinct outcome:
//!
//! ```
//! use fetchling_cli::{parse_args, ParseOutcome};
//!
//! assert!(matches!(
//!     parse_args(["--no-config", "--help"]).unwrap(),
//!     ParseOutcome::Help
//! ));
//! ```
//!
//! Unimplemented flags error; [`is_deferred_option`] detects them:
//!
//! ```
//! use fetchling_cli::{is_deferred_option, parse_args};
//! use fetchling_core::Error;
//!
//! let err = parse_args(["--no-config", "--http2"]).unwrap_err();
//! assert!(matches!(err, Error::DeferredOption(_)));
//! assert!(is_deferred_option("http2"));
//! ```

#![warn(missing_docs)]

#[cfg(doc)]
use fetchling_core::{Config, Error};

mod deferred;
mod help;
mod options;
mod parse;

pub use deferred::is_deferred_option;
pub use help::{print_help, print_version, print_version_short};
pub use parse::{parse_args, ParseOutcome};
