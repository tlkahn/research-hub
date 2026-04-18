use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error("XML parsing failed: {0}")]
    Xml(#[from] roxmltree::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Provider {provider} failed: {message}")]
    Provider { provider: String, message: String },

    #[error("Unknown citation format: {0}")]
    UnknownFormat(String),

    #[error("No PDF found for DOI {0}")]
    NoPdf(String),

    #[error("Timeout after {0}s")]
    Timeout(f64),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn provider(name: impl fmt::Display, msg: impl fmt::Display) -> Self {
        Self::Provider {
            provider: name.to_string(),
            message: msg.to_string(),
        }
    }
}
