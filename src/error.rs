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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_provider_constructor() {
        let err = Error::provider("CrossRef", "rate limited");
        match &err {
            Error::Provider { provider, message } => {
                assert_eq!(provider, "CrossRef");
                assert_eq!(message, "rate limited");
            }
            _ => panic!("expected Provider variant"),
        }
    }

    #[test]
    fn test_error_provider_display() {
        let err = Error::provider("ArXiv", "connection refused");
        let msg = format!("{err}");
        assert_eq!(msg, "Provider ArXiv failed: connection refused");
    }

    #[test]
    fn test_error_unknown_format_display() {
        let err = Error::UnknownFormat("vancouver".into());
        assert_eq!(format!("{err}"), "Unknown citation format: vancouver");
    }

    #[test]
    fn test_error_no_pdf_display() {
        let err = Error::NoPdf("10.1234/test".into());
        assert_eq!(format!("{err}"), "No PDF found for DOI 10.1234/test");
    }

    #[test]
    fn test_error_timeout_display() {
        let err = Error::Timeout(30.0);
        assert_eq!(format!("{err}"), "Timeout after 30s");
    }
}
