use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Paper {
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
    pub year: Option<i32>,
    #[serde(default)]
    pub source: String,
    pub url: Option<String>,
    pub pdf_url: Option<String>,
    pub journal: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub citation_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: String,
    pub search_type: String,
    #[serde(default)]
    pub papers: Vec<Paper>,
    #[serde(default)]
    pub total_results: usize,
    #[serde(default)]
    pub providers_searched: Vec<String>,
    #[serde(default)]
    pub providers_failed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub doi: String,
    pub success: bool,
    pub file_path: Option<String>,
    pub error: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PaperMetadata {
    pub doi: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub publisher: Option<String>,
    pub abstract_text: Option<String>,
    pub url: Option<String>,
}

impl PaperMetadata {
    pub fn new(doi: impl Into<String>) -> Self {
        let doi = doi.into();
        let url = format!("https://doi.org/{doi}");
        Self {
            doi,
            url: Some(url),
            ..Default::default()
        }
    }

    pub fn with_fallback_title(doi: impl Into<String>) -> Self {
        let doi = doi.into();
        let title = format!("Unknown paper ({doi})");
        let url = format!("https://doi.org/{doi}");
        Self {
            doi,
            title,
            url: Some(url),
            ..Default::default()
        }
    }
}
