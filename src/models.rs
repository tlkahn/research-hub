use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SortOrder {
    Relevance,
    Date,
    DateAsc,
    Citations,
}

impl std::fmt::Display for SortOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relevance => write!(f, "relevance"),
            Self::Date => write!(f, "date"),
            Self::DateAsc => write!(f, "date-asc"),
            Self::Citations => write!(f, "citations"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Paper {
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    #[serde(default)]
    pub source: String,
    pub url: Option<String>,
    pub pdf_url: Option<String>,
    pub journal: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub citation_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arxiv_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oclc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lccn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHits {
    pub provider: String,
    pub total_hits: usize,
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
    pub offset: usize,
    #[serde(default)]
    pub sort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_hits: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_hits: Vec<ProviderHits>,
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
    pub isbn: Option<String>,
    pub oclc: Option<String>,
    pub lccn: Option<String>,
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

    pub fn from_isbn(isbn: impl Into<String>) -> Self {
        Self {
            isbn: Some(isbn.into()),
            ..Default::default()
        }
    }

    pub fn from_oclc(oclc: impl Into<String>) -> Self {
        Self {
            oclc: Some(oclc.into()),
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
        assert!(p.publisher.is_none());
        assert!(p.isbn.is_none());
        assert!(p.issn.is_none());
        assert!(p.arxiv_id.is_none());
        assert!(p.work_type.is_none());
        assert!(p.editors.is_empty());
        assert!(p.series.is_none());
        assert!(p.oclc.is_none());
        assert!(p.lccn.is_none());
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
    fn test_paper_new_fields_serde_roundtrip() {
        let paper = Paper {
            title: "A Book Chapter".into(),
            publisher: Some("Springer".into()),
            isbn: Some("978-3-030-12345-6".into()),
            issn: Some("1234-5678".into()),
            arxiv_id: Some("2301.00001".into()),
            work_type: Some("book-chapter".into()),
            editors: vec!["Editor One".into(), "Editor Two".into()],
            series: Some("LNCS".into()),
            oclc: Some("123456789".into()),
            lccn: Some("2021012345".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&paper).unwrap();
        let deser: Paper = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.publisher, Some("Springer".into()));
        assert_eq!(deser.isbn, Some("978-3-030-12345-6".into()));
        assert_eq!(deser.issn, Some("1234-5678".into()));
        assert_eq!(deser.arxiv_id, Some("2301.00001".into()));
        assert_eq!(deser.work_type, Some("book-chapter".into()));
        assert_eq!(deser.editors, vec!["Editor One", "Editor Two"]);
        assert_eq!(deser.series, Some("LNCS".into()));
        assert_eq!(deser.oclc, Some("123456789".into()));
        assert_eq!(deser.lccn, Some("2021012345".into()));
    }

    #[test]
    fn test_paper_new_fields_omitted_when_default() {
        let paper = Paper {
            title: "Minimal".into(),
            source: "test".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&paper).unwrap();
        assert!(!json.contains("publisher"));
        assert!(!json.contains("isbn"));
        assert!(!json.contains("issn"));
        assert!(!json.contains("arxiv_id"));
        assert!(!json.contains("work_type"));
        assert!(!json.contains("editors"));
        assert!(!json.contains("series"));
        assert!(!json.contains("oclc"));
        assert!(!json.contains("lccn"));
    }

    #[test]
    fn test_search_result_serde() {
        let sr = SearchResult {
            query: "test".into(),
            search_type: "KEYWORDS".into(),
            papers: vec![],
            total_results: 0,
            offset: 0,
            sort: "relevance".into(),
            total_hits: None,
            provider_hits: vec![],
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
        assert!(meta.isbn.is_none());
        assert!(meta.oclc.is_none());
        assert!(meta.lccn.is_none());
    }

    #[test]
    fn test_paper_metadata_from_isbn() {
        let meta = PaperMetadata::from_isbn("978-3-030-12345-6");
        assert_eq!(meta.isbn, Some("978-3-030-12345-6".into()));
        assert!(meta.doi.is_empty());
        assert!(meta.url.is_none());
        assert!(meta.title.is_empty());
        assert!(meta.oclc.is_none());
        assert!(meta.lccn.is_none());
    }

    #[test]
    fn test_paper_metadata_from_oclc() {
        let meta = PaperMetadata::from_oclc("123456789");
        assert_eq!(meta.oclc, Some("123456789".into()));
        assert!(meta.doi.is_empty());
        assert!(meta.url.is_none());
        assert!(meta.title.is_empty());
        assert!(meta.isbn.is_none());
        assert!(meta.lccn.is_none());
    }

    #[test]
    fn test_search_result_total_hits_omitted_when_none() {
        let sr = SearchResult {
            query: "q".into(),
            search_type: "KEYWORDS".into(),
            papers: vec![],
            total_results: 0,
            offset: 0,
            sort: "relevance".into(),
            total_hits: None,
            provider_hits: vec![],
            providers_searched: vec![],
            providers_failed: vec![],
        };
        let json = serde_json::to_string(&sr).unwrap();
        assert!(!json.contains("total_hits"));
        assert!(!json.contains("provider_hits"));
    }

    #[test]
    fn test_search_result_total_hits_present_when_some() {
        let sr = SearchResult {
            query: "q".into(),
            search_type: "KEYWORDS".into(),
            papers: vec![],
            total_results: 0,
            offset: 0,
            sort: "relevance".into(),
            total_hits: Some(5000),
            provider_hits: vec![
                ProviderHits { provider: "openalex".into(), total_hits: 3000 },
                ProviderHits { provider: "crossref".into(), total_hits: 2000 },
            ],
            providers_searched: vec!["openalex".into(), "crossref".into()],
            providers_failed: vec![],
        };
        let json = serde_json::to_string(&sr).unwrap();
        assert!(json.contains("\"total_hits\":5000"));
        assert!(json.contains("\"provider_hits\""));
        assert!(json.contains("\"openalex\""));
        assert!(json.contains("3000"));
    }

    #[test]
    fn test_search_result_total_hits_roundtrip() {
        let sr = SearchResult {
            query: "transformer".into(),
            search_type: "KEYWORDS".into(),
            papers: vec![],
            total_results: 3,
            offset: 0,
            sort: "relevance".into(),
            total_hits: Some(12847),
            provider_hits: vec![
                ProviderHits { provider: "openalex".into(), total_hits: 8432 },
                ProviderHits { provider: "crossref".into(), total_hits: 3210 },
                ProviderHits { provider: "pubmed".into(), total_hits: 1205 },
            ],
            providers_searched: vec!["openalex".into(), "crossref".into(), "pubmed".into()],
            providers_failed: vec![],
        };
        let json = serde_json::to_string(&sr).unwrap();
        let deser: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.total_hits, Some(12847));
        assert_eq!(deser.provider_hits.len(), 3);
        assert_eq!(deser.provider_hits[0].provider, "openalex");
        assert_eq!(deser.provider_hits[0].total_hits, 8432);
        assert_eq!(deser.provider_hits[2].provider, "pubmed");
        assert_eq!(deser.provider_hits[2].total_hits, 1205);
    }

    #[test]
    fn test_search_result_deserialize_without_hits_fields() {
        let json = r#"{
            "query": "test",
            "search_type": "KEYWORDS",
            "papers": [],
            "total_results": 0,
            "providers_searched": [],
            "providers_failed": []
        }"#;
        let sr: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(sr.total_hits, None);
        assert!(sr.provider_hits.is_empty());
    }

    #[test]
    fn test_provider_hits_serde_roundtrip() {
        let ph = ProviderHits { provider: "core".into(), total_hits: 42 };
        let json = serde_json::to_string(&ph).unwrap();
        let deser: ProviderHits = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.provider, "core");
        assert_eq!(deser.total_hits, 42);
    }
}
