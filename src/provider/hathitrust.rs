use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, extract_year, retry};

/// Parse a catalog record from the HathiTrust Bibliographic API.
///
/// The `record` is a single value from the `records` map object.
/// The `items` slice comes from the top-level `items` array.
fn parse_catalog_entry(record: &serde_json::Value, items: &[serde_json::Value]) -> Paper {
    let title = record
        .get("titles")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let publish_date_str = record
        .get("publishDates")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str());

    let year = publish_date_str.and_then(extract_year);
    let published_date = publish_date_str.map(String::from);

    let isbn = record
        .get("isbns")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let oclc = record
        .get("oclcs")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let lccn = record
        .get("lccns")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let url = items
        .first()
        .and_then(|item| item.get("itemURL"))
        .and_then(|v| v.as_str())
        .map(String::from);

    Paper {
        title,
        year,
        published_date,
        isbn,
        oclc,
        lccn,
        url,
        source: "hathitrust".into(),
        ..Default::default()
    }
}

pub struct HathiTrustProvider {
    base: ProviderBase,
}

impl HathiTrustProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(500)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://catalog.hathitrust.org/api/volumes/brief")
    }
}

#[async_trait]
impl Provider for HathiTrustProvider {
    fn name(&self) -> &str {
        "hathitrust"
    }
    fn priority(&self) -> i32 {
        40
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Isbn]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        _limit: usize,
        _offset: usize,
    ) -> Result<ProviderResult> {
        if search_type != SearchType::Isbn {
            return Ok(ProviderResult {
                papers: vec![],
                total_hits: None,
            });
        }

        let isbn = query.replace('-', "");
        let url = format!("{}/isbn/{}.json", self.base_url(), isbn);
        let base = &self.base;

        retry("hathitrust", 3, || {
            let url = url.clone();
            async move {
                base.rate_limiter.wait().await;
                let resp = base.client.get(&url).send().await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(ProviderResult {
                        papers: vec![],
                        total_hits: None,
                    });
                }
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;

                let records = data.get("records").and_then(|v| v.as_object());
                if records.is_none_or(|m| m.is_empty()) {
                    return Ok(ProviderResult {
                        papers: vec![],
                        total_hits: None,
                    });
                }

                let record = records.unwrap().values().next().unwrap();
                let items: Vec<serde_json::Value> = data
                    .get("items")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let paper = parse_catalog_entry(record, &items);
                Ok(ProviderResult {
                    papers: vec![paper],
                    total_hits: Some(1),
                })
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_catalog_entry tests ----

    #[test]
    fn test_parse_catalog_entry_full() {
        let record = serde_json::json!({
            "titles": ["Introduction to Algorithms"],
            "publishDates": ["2005"],
            "isbns": ["9780262033848"],
            "oclcs": ["12345678"],
            "lccns": ["2004056789"]
        });
        let items = vec![serde_json::json!({
            "itemURL": "https://hdl.handle.net/2027/mdp.39015062301944"
        })];
        let paper = parse_catalog_entry(&record, &items);
        assert_eq!(paper.title, "Introduction to Algorithms");
        assert_eq!(paper.year, Some(2005));
        assert_eq!(paper.published_date, Some("2005".to_string()));
        assert_eq!(paper.isbn, Some("9780262033848".to_string()));
        assert_eq!(paper.oclc, Some("12345678".to_string()));
        assert_eq!(paper.lccn, Some("2004056789".to_string()));
        assert_eq!(
            paper.url,
            Some("https://hdl.handle.net/2027/mdp.39015062301944".to_string())
        );
        assert_eq!(paper.source, "hathitrust");
        // Verify unset fields
        assert_eq!(paper.doi, None);
        assert_eq!(paper.pdf_url, None);
        assert_eq!(paper.journal, None);
        assert_eq!(paper.volume, None);
        assert_eq!(paper.issue, None);
        assert_eq!(paper.pages, None);
        assert_eq!(paper.citation_count, None);
        assert_eq!(paper.abstract_text, None);
        assert!(paper.authors.is_empty());
    }

    #[test]
    fn test_parse_catalog_entry_minimal() {
        let record = serde_json::json!({});
        let items: Vec<serde_json::Value> = vec![];
        let paper = parse_catalog_entry(&record, &items);
        assert_eq!(paper.title, "Unknown");
        assert_eq!(paper.year, None);
        assert_eq!(paper.published_date, None);
        assert_eq!(paper.isbn, None);
        assert_eq!(paper.oclc, None);
        assert_eq!(paper.lccn, None);
        assert_eq!(paper.url, None);
        assert_eq!(paper.source, "hathitrust");
    }

    #[test]
    fn test_parse_catalog_entry_no_items() {
        let record = serde_json::json!({
            "titles": ["A Book With No Items"]
        });
        let items: Vec<serde_json::Value> = vec![];
        let paper = parse_catalog_entry(&record, &items);
        assert_eq!(paper.title, "A Book With No Items");
        assert_eq!(paper.url, None);
    }

    #[test]
    fn test_parse_catalog_entry_multiple_titles() {
        let record = serde_json::json!({
            "titles": ["First Title", "Second Title"]
        });
        let items: Vec<serde_json::Value> = vec![];
        let paper = parse_catalog_entry(&record, &items);
        assert_eq!(paper.title, "First Title");
    }

    #[test]
    fn test_parse_catalog_entry_complex_publish_date() {
        let record = serde_json::json!({
            "titles": ["Date Test"],
            "publishDates": ["c2005."]
        });
        let items: Vec<serde_json::Value> = vec![];
        let paper = parse_catalog_entry(&record, &items);
        assert_eq!(paper.year, Some(2005));
        assert_eq!(paper.published_date, Some("c2005.".to_string()));
    }

    // ---- provider metadata tests ----

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = HathiTrustProvider::new(client, config);
        assert_eq!(provider.name(), "hathitrust");
        assert_eq!(provider.priority(), 40);
        assert_eq!(provider.base_delay(), Duration::from_millis(500));
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = HathiTrustProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Isbn));
        assert!(!types.contains(&SearchType::Doi));
        assert!(!types.contains(&SearchType::Keywords));
        assert!(!types.contains(&SearchType::Author));
        assert!(!types.contains(&SearchType::Title));
    }

    #[test]
    fn test_base_url_default() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = HathiTrustProvider::new(client, config);
        assert_eq!(
            provider.base_url(),
            "https://catalog.hathitrust.org/api/volumes/brief"
        );
    }

    #[test]
    fn test_base_url_override() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = HathiTrustProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(500))
                .with_base_url("http://localhost:8080".into()),
        };
        assert_eq!(provider.base_url(), "http://localhost:8080");
    }

    // ── wiremock integration tests ──

    #[tokio::test]
    async fn isbn_lookup_end_to_end() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/isbn/9780262033848.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "records": {
                    "rec001": {
                        "titles": ["Introduction to Algorithms"],
                        "publishDates": ["2009"],
                        "isbns": ["9780262033848"],
                        "oclcs": ["318353898"],
                        "lccns": ["2009008593"]
                    }
                },
                "items": [{
                    "itemURL": "https://hdl.handle.net/2027/mdp.39015062301944"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = HathiTrustProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ).with_base_url(server.uri()),
        };

        let result = provider.search("978-0-262-03384-8", SearchType::Isbn, 10, 0).await.unwrap();

        assert_eq!(result.total_hits, Some(1));
        let p = &result.papers[0];
        assert_eq!(p.title, "Introduction to Algorithms");
        assert_eq!(p.isbn.as_deref(), Some("9780262033848"));
        assert_eq!(p.oclc.as_deref(), Some("318353898"));
        assert_eq!(p.year, Some(2009));
        assert_eq!(p.url.as_deref(), Some("https://hdl.handle.net/2027/mdp.39015062301944"));
        assert_eq!(p.source, "hathitrust");
    }

    #[tokio::test]
    async fn isbn_not_found_returns_empty() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/isbn/0000000000.json"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let provider = HathiTrustProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ).with_base_url(server.uri()),
        };

        let result = provider.search("0000000000", SearchType::Isbn, 10, 0).await.unwrap();
        assert!(result.papers.is_empty());
    }

    #[tokio::test]
    async fn empty_records_returns_empty() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/isbn/9780000000000.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "records": {},
                "items": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = HathiTrustProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ).with_base_url(server.uri()),
        };

        let result = provider.search("9780000000000", SearchType::Isbn, 10, 0).await.unwrap();
        assert!(result.papers.is_empty());
    }

    #[tokio::test]
    async fn non_isbn_search_returns_empty_without_http() {
        let provider = HathiTrustProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ),
        };

        let result = provider.search("algorithms", SearchType::Keywords, 10, 0).await.unwrap();
        assert!(result.papers.is_empty());
    }
}
