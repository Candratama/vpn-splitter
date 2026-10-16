pub mod config;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Permission denied")]
    Permission,
    #[error("Operation timed out")]
    Timeout,
    #[error("Conflicting network configuration")]
    Conflict,
    #[error("Unsupported VPN profile")]
    UnsupportedProfile,
    #[error("Hostname resolution failed")]
    Dns,
    #[error("Helper communication failed")]
    Transport,
    #[error("Route cleanup is incomplete")]
    Cleanup,
    #[error("Local file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Permission => "permission",
            Self::Timeout => "timeout",
            Self::Conflict => "conflict",
            Self::UnsupportedProfile => "unsupported_profile",
            Self::Dns => "dns",
            Self::Transport => "transport",
            Self::Cleanup => "cleanup",
            Self::Io(_) => "io",
        }
    }
}
