use std::fmt;
use std::io;

/// Every failure in tafas is one of these. There is deliberately no error-chain
/// crate: the categories are few, and each one maps onto exactly one exit code
/// and one `kind` in the JSON output.
#[derive(Debug)]
pub enum Error {
    /// The command line, or an argument's value, is wrong. Nothing was fetched.
    Usage(String),
    /// Filesystem failure: a directory that does not exist, a full disk, a
    /// rename that lost a race.
    Io(io::Error),
    /// Transport failure: DNS, TLS, a refused connection, a 5xx that outlived
    /// the retry budget.
    Http(String),
    /// A response was received and could not be understood — not valid JSON,
    /// a page whose shape changed, a container that does not parse.
    Parse(String),
    /// The request is understood and tafas declines it on purpose: age-gated
    /// video, signature-protected format, watermarked-only source. Not a crash
    /// and not a silent skip — the reason is part of the output.
    Refused(String),
    /// The site answered, but with content this build does not handle
    /// (a live stream, a photo post, a region block).
    Unsupported(String),
    /// An output file failed its contract after it was written. This is a bug
    /// in tafas, and it exits 4 so a script can tell it apart from a network
    /// error.
    Verify(String),
    /// A configured limit was exceeded (response size, media size, redirects).
    Limit(String),
    /// An invariant that cannot fail was violated.
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(m) => write!(f, "{m}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Http(m) => write!(f, "{m}"),
            Error::Parse(m) => write!(f, "{m}"),
            Error::Refused(m) => write!(f, "{m}"),
            Error::Unsupported(m) => write!(f, "{m}"),
            Error::Verify(m) => write!(f, "verification failed: {m}"),
            Error::Limit(m) => write!(f, "{m}"),
            Error::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(format!("json: {e}"))
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // The URL is part of reqwest's Display only when it is set; the chain of
        // sources is what makes a TLS or DNS failure legible, so it is appended
        // here rather than lost with the type.
        let mut text = format!("{e}");
        let mut source = std::error::Error::source(&e);
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        Error::Http(text)
    }
}

impl Error {
    /// Stable identifier: it appears in the JSON output and in `-v` logs, so a
    /// script never has to match on prose.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::Io(_) => "io",
            Error::Http(_) => "http",
            Error::Parse(_) => "parse",
            Error::Refused(_) => "refused",
            Error::Unsupported(_) => "unsupported",
            Error::Verify(_) => "verify",
            Error::Limit(_) => "limit",
            Error::Internal(_) => "internal",
        }
    }

    /// The process exit code this error earns, given that a run may have
    /// already succeeded at something else.
    ///
    /// `4` is reserved for a verification failure, because that is the only
    /// class of failure that means the program wrote a file it does not trust;
    /// `2` never coexists with any work, because it is only produced before the
    /// first request.
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Usage(_) => 2,
            Error::Verify(_) => 4,
            Error::Internal(_) => 3,
            _ => 1,
        }
    }

    pub fn is_usage(&self) -> bool {
        matches!(self, Error::Usage(_))
    }

    pub fn is_parse(&self) -> bool {
        matches!(self, Error::Parse(_))
    }
}
