use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Generic = 1,
    Parse = 2,
    Io = 3,
    Network = 4,
    Ssl = 5,
    Auth = 6,
    Protocol = 7,
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

    pub fn worse(self, other: ExitCode) -> ExitCode {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),

    #[error("invalid option: {0}")]
    InvalidOption(String),

    #[error("deferred option: --{0}")]
    DeferredOption(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("network error: {0}")]
    Network(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("auth error: {0}")]
    Auth(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
}

impl Error {
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
    fn network_error_maps_to_network_exit() {
        let e = Error::Network("connect refused".into());
        assert_eq!(e.exit_code(), ExitCode::Network);
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
