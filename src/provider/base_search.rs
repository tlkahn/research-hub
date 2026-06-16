use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

/// Handle BASE API fields that can be either a single string or an array of strings.
fn flex_string_or_array(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        None => vec![],
        Some(v) => {
            if let Some(s) = v.as_str() {
                vec![s.to_string()]
            } else if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect()
            } else {
                vec![]
            }
        }
    }
}

/// Get the first string from a possibly polymorphic field.
fn flex_first_string(value: Option<&serde_json::Value>) -> Option<String> {
    flex_string_or_array(value).into_iter().next()
}

/// Extract a DOI from a list of identifiers (dcidentifier values).
fn extract_doi(identifiers: &[String]) -> Option<String> {
    for id in identifiers {
        // Check for bare DOI starting with "10."
        if id.starts_with("10.") {
            return Some(id.clone());
        }
        // Check for DOI in URL form
        if let Some(rest) = id
            .strip_prefix("https://doi.org/")
            .or_else(|| id.strip_prefix("http://doi.org/"))
            .filter(|r| r.starts_with("10."))
        {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse a single BASE document record into a Paper.
fn parse_record(doc: &serde_json::Value) -> Paper {
    let title = flex_first_string(doc.get("dctitle"))
        .unwrap_or_else(|| "Unknown".into());

    let authors = flex_string_or_array(doc.get("dcauthor"));

    let year = doc.get("dcyear").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
    }).map(|y| y as i32);

    let published_date = year.map(|y| format!("{y:04}-01-01"));

    let identifiers = flex_string_or_array(doc.get("dcidentifier"));
    let doi = extract_doi(&identifiers);

    let abstract_text = flex_first_string(doc.get("dcdescription"))
        .filter(|s| !s.is_empty());

    let publisher = flex_first_string(doc.get("dcpublisher"));
    let work_type = flex_first_string(doc.get("dctype"));

    let url = doc
        .get("dclink")
        .and_then(|v| v.as_str())
        .map(String::from);

    Paper {
        title,
        authors,
        year,
        published_date,
        doi,
        abstract_text,
        publisher,
        work_type,
        url,
        source: "base".into(),
        ..Default::default()
    }
}

pub struct BaseSearchProvider {
    base: ProviderBase,
}

impl BaseSearchProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(1000)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.base-search.net")
    }
}

#[async_trait]
impl Provider for BaseSearchProvider {
    fn name(&self) -> &str {
        "base"
    }

    fn priority(&self) -> i32 {
        50
    }

    fn base_delay(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Keywords, SearchType::Title, SearchType::Author]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
        offset: usize,
    ) -> Result<ProviderResult> {
        // Early return if no API key configured
        let api_key = match self.base.config.base_api_key.clone() {
            Some(key) => key,
            None => {
                return Ok(ProviderResult {
                    papers: vec![],
                    total_hits: None,
                });
            }
        };

        // Early return for unsupported search types
        if !self.supported_search_types().contains(&search_type) {
            return Ok(ProviderResult {
                papers: vec![],
                total_hits: None,
            });
        }

        let constructed_query = match search_type {
            SearchType::Title => format!("dctitle:{query}"),
            SearchType::Author => format!("dcauthor:{query}"),
            _ => query.to_string(),
        };

        let base = &self.base;
        let base_url = self.base_url().to_string();

        retry("base", 3, || {
            let base_url = base_url.clone();
            let constructed_query = constructed_query.clone();
            let api_key = api_key.clone();
            async move {
                base.rate_limiter.wait().await;

                let url = format!(
                    "{base_url}/cgi-bin/BaseHttpSearchInterface.fcgi"
                );
                let params: Vec<(&str, String)> = vec![
                    ("func", "PerformSearch".into()),
                    ("format", "json".into()),
                    ("query", constructed_query),
                    ("hits", limit.to_string()),
                    ("offset", offset.to_string()),
                    ("cred", api_key),
                ];

                let resp = base.client.get(&url).query(&params).send().await?;
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;

                let total_hits = data
                    .get("response")
                    .and_then(|r| r.get("numFound"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let docs = data
                    .get("response")
                    .and_then(|r| r.get("docs"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                let papers = docs.iter().take(limit).map(parse_record).collect();

                Ok(ProviderResult { papers, total_hits })
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- flex_string_or_array tests ----

    #[test]
    fn test_flex_string_or_array_none() {
        assert_eq!(flex_string_or_array(None), Vec::<String>::new());
    }

    #[test]
    fn test_flex_string_or_array_string() {
        let v = serde_json::json!("hello");
        assert_eq!(flex_string_or_array(Some(&v)), vec!["hello"]);
    }

    #[test]
    fn test_flex_string_or_array_array() {
        let v = serde_json::json!(["a", "b"]);
        assert_eq!(flex_string_or_array(Some(&v)), vec!["a", "b"]);
    }

    #[test]
    fn test_flex_string_or_array_empty_array() {
        let v = serde_json::json!([]);
        assert_eq!(flex_string_or_array(Some(&v)), Vec::<String>::new());
    }

    #[test]
    fn test_flex_string_or_array_number_ignored() {
        let v = serde_json::json!(42);
        assert_eq!(flex_string_or_array(Some(&v)), Vec::<String>::new());
    }

    #[test]
    fn test_flex_string_or_array_mixed_array() {
        let v = serde_json::json!(["a", 42, "b"]);
        assert_eq!(flex_string_or_array(Some(&v)), vec!["a", "b"]);
    }

    // ---- flex_first_string tests ----

    #[test]
    fn test_flex_first_string_none() {
        assert_eq!(flex_first_string(None), None);
    }

    #[test]
    fn test_flex_first_string_string() {
        let v = serde_json::json!("hello");
        assert_eq!(flex_first_string(Some(&v)), Some("hello".to_string()));
    }

    #[test]
    fn test_flex_first_string_array() {
        let v = serde_json::json!(["first", "second"]);
        assert_eq!(flex_first_string(Some(&v)), Some("first".to_string()));
    }

    #[test]
    fn test_flex_first_string_empty_array() {
        let v = serde_json::json!([]);
        assert_eq!(flex_first_string(Some(&v)), None);
    }

    // ---- extract_doi tests ----

    #[test]
    fn test_extract_doi_bare() {
        let ids = vec!["10.1234/test.5678".to_string()];
        assert_eq!(extract_doi(&ids), Some("10.1234/test.5678".to_string()));
    }

    #[test]
    fn test_extract_doi_with_url_prefix() {
        let ids = vec!["https://doi.org/10.1234/test".to_string()];
        assert_eq!(extract_doi(&ids), Some("10.1234/test".to_string()));
    }

    #[test]
    fn test_extract_doi_http_prefix() {
        let ids = vec!["http://doi.org/10.1234/test".to_string()];
        assert_eq!(extract_doi(&ids), Some("10.1234/test".to_string()));
    }

    #[test]
    fn test_extract_doi_not_found() {
        let ids = vec![
            "https://example.com/paper".to_string(),
            "urn:nbn:de:123".to_string(),
        ];
        assert_eq!(extract_doi(&ids), None);
    }

    #[test]
    fn test_extract_doi_multiple_picks_first() {
        let ids = vec![
            "https://example.com".to_string(),
            "10.1234/first".to_string(),
            "10.5678/second".to_string(),
        ];
        assert_eq!(extract_doi(&ids), Some("10.1234/first".to_string()));
    }

    #[test]
    fn test_extract_doi_empty() {
        let ids: Vec<String> = vec![];
        assert_eq!(extract_doi(&ids), None);
    }

    // ---- parse_record tests ----

    #[test]
    fn test_parse_record_full() {
        let doc = serde_json::json!({
            "dctitle": ["Machine Learning for NLP"],
            "dcauthor": ["Smith, John", "Doe, Jane"],
            "dcyear": 2023,
            "dcidentifier": ["https://doi.org/10.1234/ml.nlp", "oai:repo:12345"],
            "dcdescription": ["This paper explores ML for NLP tasks."],
            "dcpublisher": ["Springer"],
            "dctype": ["article"],
            "dclink": "https://example.com/paper/12345"
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.title, "Machine Learning for NLP");
        assert_eq!(paper.authors, vec!["Smith, John", "Doe, Jane"]);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.doi, Some("10.1234/ml.nlp".to_string()));
        assert_eq!(
            paper.abstract_text,
            Some("This paper explores ML for NLP tasks.".to_string())
        );
        assert_eq!(paper.publisher, Some("Springer".to_string()));
        assert_eq!(paper.work_type, Some("article".to_string()));
        assert_eq!(
            paper.url,
            Some("https://example.com/paper/12345".to_string())
        );
        assert_eq!(paper.source, "base");
        assert_eq!(paper.published_date, Some("2023-01-01".to_string()));
        // Fields that should be None/default
        assert_eq!(paper.pdf_url, None);
        assert_eq!(paper.journal, None);
        assert_eq!(paper.volume, None);
        assert_eq!(paper.issue, None);
        assert_eq!(paper.pages, None);
        assert_eq!(paper.citation_count, None);
        assert_eq!(paper.isbn, None);
        assert_eq!(paper.issn, None);
        assert_eq!(paper.arxiv_id, None);
        assert!(paper.editors.is_empty());
        assert_eq!(paper.series, None);
        assert_eq!(paper.oclc, None);
        assert_eq!(paper.lccn, None);
    }

    #[test]
    fn test_parse_record_string_fields() {
        let doc = serde_json::json!({
            "dctitle": "A Single Title",
            "dcauthor": "Solo Author",
            "dcyear": "2020"
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.title, "A Single Title");
        assert_eq!(paper.authors, vec!["Solo Author"]);
        assert_eq!(paper.year, Some(2020));
    }

    #[test]
    fn test_parse_record_minimal() {
        let doc = serde_json::json!({
            "dctitle": "Just a Title"
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.title, "Just a Title");
        assert!(paper.authors.is_empty());
        assert_eq!(paper.year, None);
        assert_eq!(paper.published_date, None);
        assert_eq!(paper.doi, None);
        assert_eq!(paper.abstract_text, None);
        assert_eq!(paper.publisher, None);
        assert_eq!(paper.work_type, None);
        assert_eq!(paper.url, None);
        assert_eq!(paper.source, "base");
    }

    #[test]
    fn test_parse_record_no_title() {
        let doc = serde_json::json!({});
        let paper = parse_record(&doc);
        assert_eq!(paper.title, "Unknown");
    }

    #[test]
    fn test_parse_record_no_doi_in_identifiers() {
        let doc = serde_json::json!({
            "dctitle": "Test",
            "dcidentifier": ["oai:repo:12345", "urn:nbn:de:999"]
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.doi, None);
    }

    #[test]
    fn test_parse_record_year_as_string() {
        let doc = serde_json::json!({
            "dctitle": "Test",
            "dcyear": "2019"
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.year, Some(2019));
        assert_eq!(paper.published_date, Some("2019-01-01".to_string()));
    }

    #[test]
    fn test_parse_record_year_as_number() {
        let doc = serde_json::json!({
            "dctitle": "Test",
            "dcyear": 2019
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.year, Some(2019));
        assert_eq!(paper.published_date, Some("2019-01-01".to_string()));
    }

    #[test]
    fn test_parse_record_empty_description_filtered() {
        let doc = serde_json::json!({
            "dctitle": "Test",
            "dcdescription": [""]
        });
        let paper = parse_record(&doc);
        assert_eq!(paper.abstract_text, None);
    }

    // ---- provider metadata tests ----

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        assert_eq!(provider.name(), "base");
        assert_eq!(provider.priority(), 50);
        assert_eq!(provider.base_delay(), Duration::from_millis(1000));
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Keywords));
        assert!(types.contains(&SearchType::Title));
        assert!(types.contains(&SearchType::Author));
        assert!(!types.contains(&SearchType::Doi));
        assert!(!types.contains(&SearchType::Isbn));
    }

    #[test]
    fn test_base_url_default() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        assert_eq!(provider.base_url(), "https://api.base-search.net");
    }

    #[test]
    fn test_base_url_override() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(1000))
                .with_base_url("http://localhost:8080".into()),
        };
        assert_eq!(provider.base_url(), "http://localhost:8080");
    }

    // ---- search early-return tests ----

    #[tokio::test]
    async fn test_search_returns_empty_without_api_key() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        let result = provider
            .search("test", SearchType::Keywords, 10, 0)
            .await
            .unwrap();
        assert!(result.papers.is_empty());
        assert_eq!(result.total_hits, None);
    }

    #[tokio::test]
    async fn test_search_returns_empty_for_unsupported_type() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        let result = provider
            .search("test", SearchType::Doi, 10, 0)
            .await
            .unwrap();
        assert!(result.papers.is_empty());
        assert_eq!(result.total_hits, None);
    }

    #[tokio::test]
    async fn test_search_returns_empty_for_isbn() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = BaseSearchProvider::new(client, config);
        let result = provider
            .search("978-0-123", SearchType::Isbn, 10, 0)
            .await
            .unwrap();
        assert!(result.papers.is_empty());
        assert_eq!(result.total_hits, None);
    }
}
