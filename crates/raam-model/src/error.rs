//! Typed errors. Never classify an error by matching a string
//! (TIGERSTYLE.md); the variant is the classification.

/// A provider call's failure. `Transport` is the engine's offline
/// signal: the server could not be reached at all (DNS, connect, TLS,
/// timeout, a broken transfer). Everything else — an HTTP error status,
/// a payload that didn't parse, local I/O — is `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    Transport(String),
    Failed(String),
}

impl ProviderError {
    pub fn is_transport(&self) -> bool {
        matches!(self, ProviderError::Transport(_))
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Transport(s) | ProviderError::Failed(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for ProviderError {}
