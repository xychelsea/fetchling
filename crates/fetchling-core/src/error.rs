//! Process exit codes and the crate [`Error`](crate::Error) type.

use std::fmt;

/// Process exit status codes (wget-compatible numeric values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Successful completion.
    Success = 0,
    /// Unspecified or generic failure.
    Generic = 1,
    /// CLI/config/URL parse failure.
    Parse = 2,
    /// Local filesystem or I/O failure.
    Io = 3,
    /// Network connectivity or DNS failure.
    Network = 4,
    /// TLS/SSL failure.
    Ssl = 5,
    /// Authentication failure.
    Auth = 6,
    /// Protocol-level failure.
    Protocol = 7,
    /// Remote server error response.
    Server = 8,
}

impl From<ExitCode> for i32 {
    fn from(c: ExitCode) -> Self {
        c as i32
    }
}

impl ExitCode {
    fn severity(self) -> u8 {
        match self {
            ExitCode::Success => 0,
            ExitCode::Generic => 1,
            ExitCode::Parse => 2,
            ExitCode::Server => 3,
            ExitCode::Io => 4,
            ExitCode::Protocol => 5,
            ExitCode::Network => 6,
            ExitCode::Ssl => 7,
            ExitCode::Auth => 8,
        }
    }

    /// Return the more severe of `self` and `other` (for aggregating multi-URL runs).
    ///
    /// Severity order (highest first): Auth, Ssl, Network, Protocol, Io, Server,
    /// Parse, Generic, Success.
    pub fn worse(self, other: ExitCode) -> ExitCode {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }
}

/// Error type for fetchling core and dependent crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Generic message without a more specific category.
    #[error("{0}")]
    Message(String),

    /// Unknown or invalid CLI/config option.
    #[error("invalid option: {0}")]
    InvalidOption(String),

    /// Recognized but intentionally deferred/unimplemented option.
    #[error("deferred option: --{0}")]
    DeferredOption(String),

    /// Value parse failure (bytes, duration, URL text, …).
    #[error("parse error: {0}")]
    Parse(String),

    /// Local I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Network/DNS/connect failure.
    #[error("network error: {0}")]
    Network(String),

    /// TLS failure.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Authentication failure.
    #[error("auth error: {0}")]
    Auth(String),

    /// Protocol misuse or unexpected protocol state.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// HTTP/FTP server error or unexpected status.
    #[error("server error: {0}")]
    Server(String),

    /// [`url::Url`] parse failure.
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
}

impl Error {
    /// Map this error to a process [`ExitCode`].
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Error::InvalidOption(_) | Error::DeferredOption(_) | Error::Parse(_) => ExitCode::Parse,
            Error::Io(_) => ExitCode::Io,
            Error::Network(_) => ExitCode::Network,
            Error::Tls(_) => ExitCode::Ssl,
            Error::Auth(_) => ExitCode::Auth,
            Error::Protocol(_) => ExitCode::Protocol,
            Error::Server(_) => ExitCode::Server,
            Error::Url(_) => ExitCode::Parse,
            Error::Message(_) => ExitCode::Generic,
        }
    }

    /// Whether this looks like a host-level DNS failure (retry-on-host-error).
    pub fn is_host_error(&self) -> bool {
        match self {
            Error::Network(s) => {
                s.contains("DNS lookup failed")
                    || s.contains("DNS timeout")
                    || s.starts_with("no addresses for")
            }
            _ => false,
        }
    }
}

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worse_prefers_network_over_server() {
        assert_eq!(ExitCode::Server.worse(ExitCode::Network), ExitCode::Network);
        assert_eq!(ExitCode::Network.worse(ExitCode::Server), ExitCode::Network);
    }

    #[test]
    fn worse_prefers_auth_highest() {
        assert_eq!(ExitCode::Ssl.worse(ExitCode::Auth), ExitCode::Auth);
        assert_eq!(ExitCode::Auth.worse(ExitCode::Network), ExitCode::Auth);
    }

    #[test]
    fn worse_severity_order_and_identity() {
        let order = [
            ExitCode::Success,
            ExitCode::Generic,
            ExitCode::Parse,
            ExitCode::Server,
            ExitCode::Io,
            ExitCode::Protocol,
            ExitCode::Network,
            ExitCode::Ssl,
            ExitCode::Auth,
        ];
        for window in order.windows(2) {
            assert_eq!(window[0].worse(window[1]), window[1]);
            assert_eq!(window[1].worse(window[0]), window[1]);
        }
        for &code in &order {
            assert_eq!(code.worse(code), code);
        }
    }

    #[test]
    fn exit_code_display_is_numeric() {
        assert_eq!(ExitCode::Success.to_string(), "0");
        assert_eq!(ExitCode::Network.to_string(), "4");
        assert_eq!(ExitCode::Auth.to_string(), "6");
    }

    #[test]
    fn error_variants_map_to_exit_codes() {
        assert_eq!(
            Error::InvalidOption("x".into()).exit_code(),
            ExitCode::Parse
        );
        assert_eq!(
            Error::DeferredOption("y".into()).exit_code(),
            ExitCode::Parse
        );
        assert_eq!(Error::Parse("z".into()).exit_code(), ExitCode::Parse);
        assert_eq!(
            Error::Io(std::io::Error::other("io")).exit_code(),
            ExitCode::Io
        );
        assert_eq!(
            Error::Network("connect refused".into()).exit_code(),
            ExitCode::Network
        );
        assert_eq!(Error::Tls("handshake".into()).exit_code(), ExitCode::Ssl);
        assert_eq!(Error::Auth("denied".into()).exit_code(), ExitCode::Auth);
        assert_eq!(
            Error::Protocol("bad".into()).exit_code(),
            ExitCode::Protocol
        );
        assert_eq!(Error::Server("500".into()).exit_code(), ExitCode::Server);
        assert_eq!(
            Error::Url(url::ParseError::EmptyHost).exit_code(),
            ExitCode::Parse
        );
        assert_eq!(Error::Message("oops".into()).exit_code(), ExitCode::Generic);
    }

    #[test]
    fn host_error_classification() {
        assert!(
            Error::Network("DNS lookup failed for example.com:443: ...".into()).is_host_error()
        );
        assert!(Error::Network("DNS timeout for example.com".into()).is_host_error());
        assert!(Error::Network("no addresses for example.com:80".into()).is_host_error());
        assert!(!Error::Network("connect refused".into()).is_host_error());
        assert!(!Error::Server("500".into()).is_host_error());
    }
}
