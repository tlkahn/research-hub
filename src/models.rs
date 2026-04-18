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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paper_default() {
        let p = Paper::default();
        assert!(p.title.is_empty());
        assert!(p.authors.is_empty());
        assert!(p.doi.is_none());
        assert!(p.year.is_none());
        assert!(p.pdf_url.is_none());
    }

    #[test]
    fn test_paper_serde_roundtrip() {
        let paper = Paper {
            title: "Attention Is All You Need".into(),
            authors: vec!["Vaswani".into(), "Shazeer".into()],
            doi: Some("10.5555/3295222.3295349".into()),
            year: Some(2017),
            source: "arxiv".into(),
            url: Some("https://arxiv.org/abs/1706.03762".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&paper).unwrap();
        let deser: Paper = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.title, paper.title);
        assert_eq!(deser.authors, paper.authors);
        assert_eq!(deser.doi, paper.doi);
        assert_eq!(deser.year, paper.year);
    }

    #[test]
    fn test_paper_abstract_rename() {
        let json = r#"{"title":"T","abstract":"some text","source":""}"#;
        let paper: Paper = serde_json::from_str(json).unwrap();
        assert_eq!(paper.abstract_text, Some("some text".into()));

        let serialized = serde_json::to_string(&paper).unwrap();
        assert!(serialized.contains("\"abstract\""));
        assert!(!serialized.contains("\"abstract_text\""));
    }

    #[test]
    fn test_paper_missing_optional_fields() {
        let json = r#"{"title":"Minimal","source":"test"}"#;
        let paper: Paper = serde_json::from_str(json).unwrap();
        assert_eq!(paper.title, "Minimal");
        assert!(paper.authors.is_empty());
        assert!(paper.doi.is_none());
        assert!(paper.citation_count.is_none());
    }

    #[test]
    fn test_search_result_serde() {
        let sr = SearchResult {
            query: "test".into(),
            search_type: "KEYWORDS".into(),
            papers: vec![],
            total_results: 0,
            providers_searched: vec!["openalex".into()],
            providers_failed: vec![],
        };
        let json = serde_json::to_string(&sr).unwrap();
        let deser: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.query, "test");
        assert_eq!(deser.providers_searched, vec!["openalex"]);
    }

    #[test]
    fn test_download_result_serde() {
        let dr = DownloadResult {
            doi: "10.1234/test".into(),
            success: true,
            file_path: Some("/tmp/paper.pdf".into()),
            error: None,
            source: Some("unpaywall".into()),
        };
        let json = serde_json::to_string(&dr).unwrap();
        let deser: DownloadResult = serde_json::from_str(&json).unwrap();
        assert!(deser.success);
        assert_eq!(deser.source, Some("unpaywall".into()));
    }

    #[test]
    fn test_paper_metadata_new() {
        let meta = PaperMetadata::new("10.1234/test");
        assert_eq!(meta.doi, "10.1234/test");
        assert_eq!(meta.url, Some("https://doi.org/10.1234/test".into()));
        assert!(meta.title.is_empty());
        assert!(meta.authors.is_empty());
        assert!(meta.year.is_none());
    }

    #[test]
    fn test_paper_metadata_with_fallback_title() {
        let meta = PaperMetadata::with_fallback_title("10.1234/test");
        assert_eq!(meta.doi, "10.1234/test");
        assert_eq!(meta.title, "Unknown paper (10.1234/test)");
        assert_eq!(meta.url, Some("https://doi.org/10.1234/test".into()));
    }

    #[test]
    fn test_paper_metadata_default() {
        let meta = PaperMetadata::default();
        assert!(meta.doi.is_empty());
        assert!(meta.title.is_empty());
        assert!(meta.url.is_none());
    }
}
