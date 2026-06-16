use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

/// Pick the best ISBN from Google Books `industryIdentifiers` array.
/// Prefers ISBN_13 over ISBN_10.
fn pick_isbn(identifiers: &[serde_json::Value]) -> Option<String> {
    // First pass: look for ISBN_13
    let isbn13 = identifiers.iter().find(|id| {
        id.get("type").and_then(|v| v.as_str()) == Some("ISBN_13")
    });
    if let Some(id) = isbn13 {
        return id.get("identifier").and_then(|v| v.as_str()).map(String::from);
    }
    // Fallback: ISBN_10
    let isbn10 = identifiers.iter().find(|id| {
        id.get("type").and_then(|v| v.as_str()) == Some("ISBN_10")
    });
    isbn10.and_then(|id| id.get("identifier").and_then(|v| v.as_str()).map(String::from))
}

/// Parse a single volume item from the Google Books API response.
fn parse_volume(item: &serde_json::Value) -> Paper {
    let info = item.get("volumeInfo").cloned().unwrap_or_default();

    let title = info
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let authors: Vec<String> = info
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let publisher = info
        .get("publisher")
        .and_then(|v| v.as_str())
        .map(String::from);

    let raw_date = info
        .get("publishedDate")
        .and_then(|v| v.as_str());

    let year = raw_date
        .and_then(|d| d.get(..4))
        .and_then(|s| s.parse::<i32>().ok());

    let published_date = raw_date.map(String::from);

    let abstract_text = info
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);

    let isbn = info
        .get("industryIdentifiers")
        .and_then(|v| v.as_array())
        .and_then(|arr| pick_isbn(arr));

    let work_type = info
        .get("printType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase());

    let url = info
        .get("infoLink")
        .and_then(|v| v.as_str())
        .map(String::from);

    Paper {
        title,
        authors,
        abstract_text,
        year,
        published_date,
        publisher,
        isbn,
        work_type,
        url,
        source: "google_books".into(),
        ..Default::default()
    }
}

pub struct GoogleBooksProvider {
    base: ProviderBase,
}

impl GoogleBooksProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(200)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://www.googleapis.com")
    }
}

#[async_trait]
impl Provider for GoogleBooksProvider {
    fn name(&self) -> &str {
        "google_books"
    }
    fn priority(&self) -> i32 {
        55
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[
            SearchType::Keywords,
            SearchType::Title,
            SearchType::Author,
            SearchType::Isbn,
        ]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
        offset: usize,
    ) -> Result<ProviderResult> {
        if !self.supported_search_types().contains(&search_type) {
            return Ok(ProviderResult {
                papers: vec![],
                total_hits: None,
            });
        }

        let q_value = match search_type {
            SearchType::Title => format!("intitle:{query}"),
            SearchType::Author => format!("inauthor:{query}"),
            SearchType::Isbn => format!("isbn:{}", query.replace('-', "")),
            _ => query.to_string(), // Keywords
        };

        let base = &self.base;
        let base_url = self.base_url().to_string();

        retry("google_books", 3, || {
            let base_url = base_url.clone();
            let q_value = q_value.clone();
            async move {
                base.rate_limiter.wait().await;

                let url = format!("{base_url}/books/v1/volumes");
                let mut params: Vec<(&str, String)> = vec![
                    ("q", q_value),
                    ("startIndex", offset.to_string()),
                    ("maxResults", limit.min(40).to_string()),
                ];
                if let Some(ref api_key) = base.config.google_books_api_key {
                    params.push(("key", api_key.clone()));
                }

                let resp = base.client.get(&url).query(&params).send().await?;
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;

                let total_hits = data
                    .get("totalItems")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let papers = data
                    .get("items")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().map(parse_volume).collect())
                    .unwrap_or_default();

                Ok(ProviderResult { papers, total_hits })
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pick_isbn tests ----

    #[test]
    fn test_pick_isbn_empty() {
        let ids: Vec<serde_json::Value> = vec![];
        assert_eq!(pick_isbn(&ids), None);
    }

    #[test]
    fn test_pick_isbn_isbn13_preferred() {
        let ids = vec![
            serde_json::json!({"type": "ISBN_10", "identifier": "0262033844"}),
            serde_json::json!({"type": "ISBN_13", "identifier": "9780262033848"}),
        ];
        assert_eq!(pick_isbn(&ids), Some("9780262033848".to_string()));
    }

    #[test]
    fn test_pick_isbn_isbn10_fallback() {
        let ids = vec![
            serde_json::json!({"type": "ISBN_10", "identifier": "0262033844"}),
        ];
        assert_eq!(pick_isbn(&ids), Some("0262033844".to_string()));
    }

    #[test]
    fn test_pick_isbn_other_type_ignored() {
        let ids = vec![
            serde_json::json!({"type": "OTHER", "identifier": "ABCD1234"}),
        ];
        assert_eq!(pick_isbn(&ids), None);
    }

    // ---- parse_volume tests ----

    #[test]
    fn test_parse_volume_full() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Introduction to Algorithms",
                "authors": ["Thomas H. Cormen", "Charles E. Leiserson"],
                "publisher": "MIT Press",
                "publishedDate": "2009-07-31",
                "description": "A comprehensive textbook on algorithms.",
                "industryIdentifiers": [
                    {"type": "ISBN_10", "identifier": "0262033844"},
                    {"type": "ISBN_13", "identifier": "9780262033848"}
                ],
                "printType": "BOOK",
                "infoLink": "https://books.google.com/books?id=aefUBQAAQBAJ"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.title, "Introduction to Algorithms");
        assert_eq!(paper.authors, vec!["Thomas H. Cormen", "Charles E. Leiserson"]);
        assert_eq!(paper.publisher, Some("MIT Press".to_string()));
        assert_eq!(paper.year, Some(2009));
        assert_eq!(paper.published_date, Some("2009-07-31".to_string()));
        assert_eq!(paper.abstract_text, Some("A comprehensive textbook on algorithms.".to_string()));
        assert_eq!(paper.isbn, Some("9780262033848".to_string()));
        assert_eq!(paper.work_type, Some("book".to_string()));
        assert_eq!(paper.url, Some("https://books.google.com/books?id=aefUBQAAQBAJ".to_string()));
        assert_eq!(paper.source, "google_books");
        // Fields that should be None/default
        assert_eq!(paper.doi, None);
        assert_eq!(paper.pdf_url, None);
        assert_eq!(paper.journal, None);
        assert_eq!(paper.volume, None);
        assert_eq!(paper.issue, None);
        assert_eq!(paper.pages, None);
        assert_eq!(paper.citation_count, None);
        assert_eq!(paper.issn, None);
        assert_eq!(paper.arxiv_id, None);
        assert!(paper.editors.is_empty());
        assert_eq!(paper.series, None);
        assert_eq!(paper.oclc, None);
        assert_eq!(paper.lccn, None);
    }

    #[test]
    fn test_parse_volume_minimal() {
        let item = serde_json::json!({
            "volumeInfo": {}
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.title, "Unknown");
        assert!(paper.authors.is_empty());
        assert_eq!(paper.publisher, None);
        assert_eq!(paper.year, None);
        assert_eq!(paper.published_date, None);
        assert_eq!(paper.abstract_text, None);
        assert_eq!(paper.isbn, None);
        assert_eq!(paper.work_type, None);
        assert_eq!(paper.url, None);
        assert_eq!(paper.source, "google_books");
    }

    #[test]
    fn test_parse_volume_no_volume_info() {
        let item = serde_json::json!({});
        let paper = parse_volume(&item);
        assert_eq!(paper.title, "Unknown");
        assert!(paper.authors.is_empty());
        assert_eq!(paper.source, "google_books");
    }

    #[test]
    fn test_parse_volume_isbn_prefers_isbn13() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "industryIdentifiers": [
                    {"type": "ISBN_10", "identifier": "0262033844"},
                    {"type": "ISBN_13", "identifier": "9780262033848"}
                ]
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.isbn, Some("9780262033848".to_string()));
    }

    #[test]
    fn test_parse_volume_isbn_fallback_isbn10() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "industryIdentifiers": [
                    {"type": "ISBN_10", "identifier": "0262033844"}
                ]
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.isbn, Some("0262033844".to_string()));
    }

    #[test]
    fn test_parse_volume_no_identifiers() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "No ISBN"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.isbn, None);
    }

    #[test]
    fn test_parse_volume_published_date_full() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "publishedDate": "2023-06-15"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023-06-15".to_string()));
    }

    #[test]
    fn test_parse_volume_published_date_year_only() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "publishedDate": "2023"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023".to_string()));
    }

    #[test]
    fn test_parse_volume_published_date_year_month() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "publishedDate": "2023-06"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023-06".to_string()));
    }

    #[test]
    fn test_parse_volume_no_date() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.year, None);
        assert_eq!(paper.published_date, None);
    }

    #[test]
    fn test_parse_volume_work_type_lowercased() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "printType": "BOOK"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.work_type, Some("book".to_string()));
    }

    #[test]
    fn test_parse_volume_work_type_magazine() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "printType": "MAGAZINE"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.work_type, Some("magazine".to_string()));
    }

    #[test]
    fn test_parse_volume_description_as_abstract() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "description": "This is a description."
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.abstract_text, Some("This is a description.".to_string()));
    }

    #[test]
    fn test_parse_volume_source_is_google_books() {
        let item = serde_json::json!({
            "volumeInfo": {"title": "Test"}
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.source, "google_books");
    }

    #[test]
    fn test_parse_volume_url_from_infolink() {
        let item = serde_json::json!({
            "volumeInfo": {
                "title": "Test",
                "infoLink": "https://books.google.com/books?id=abc123"
            }
        });
        let paper = parse_volume(&item);
        assert_eq!(paper.url, Some("https://books.google.com/books?id=abc123".to_string()));
    }

    // ---- provider metadata tests ----

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider::new(client, config);
        assert_eq!(provider.name(), "google_books");
        assert_eq!(provider.priority(), 55);
        assert_eq!(provider.base_delay(), Duration::from_millis(200));
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Keywords));
        assert!(types.contains(&SearchType::Title));
        assert!(types.contains(&SearchType::Author));
        assert!(types.contains(&SearchType::Isbn));
        assert!(!types.contains(&SearchType::Doi));
    }

    #[test]
    fn test_base_url_default() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider::new(client, config);
        assert_eq!(provider.base_url(), "https://www.googleapis.com");
    }

    #[test]
    fn test_base_url_override() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(200))
                .with_base_url("http://localhost:8080".into()),
        };
        assert_eq!(provider.base_url(), "http://localhost:8080");
    }

    // ---- limit clamping test (wiremock) ----

    #[tokio::test]
    async fn test_max_results_clamped_to_40() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, query_param};

        let mock_server = MockServer::start().await;

        // Expect maxResults=40 even though we request limit=100
        Mock::given(method("GET"))
            .and(path("/books/v1/volumes"))
            .and(query_param("maxResults", "40"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "totalItems": 0,
                "items": []
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(0))
                .with_base_url(mock_server.uri()),
        };

        let result = provider
            .search("algorithms", SearchType::Keywords, 100, 0)
            .await
            .expect("search should succeed");

        assert_eq!(result.total_hits, Some(0));
        assert!(result.papers.is_empty());
    }

    #[tokio::test]
    async fn test_max_results_under_40_unchanged() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, query_param};

        let mock_server = MockServer::start().await;

        // When limit=10, maxResults should be 10 (not clamped)
        Mock::given(method("GET"))
            .and(path("/books/v1/volumes"))
            .and(query_param("maxResults", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "totalItems": 1,
                "items": [{
                    "volumeInfo": {
                        "title": "Test Book"
                    }
                }]
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = GoogleBooksProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(0))
                .with_base_url(mock_server.uri()),
        };

        let result = provider
            .search("test", SearchType::Keywords, 10, 0)
            .await
            .expect("search should succeed");

        assert_eq!(result.total_hits, Some(1));
        assert_eq!(result.papers.len(), 1);
        assert_eq!(result.papers[0].title, "Test Book");
    }

    // ── additional wiremock integration tests ──

    #[tokio::test]
    async fn isbn_search_end_to_end() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, query_param};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/books/v1/volumes"))
            .and(query_param("q", "isbn:9780262033848"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "totalItems": 1,
                "items": [{
                    "volumeInfo": {
                        "title": "Introduction to Algorithms",
                        "authors": ["Thomas H. Cormen"],
                        "publisher": "MIT Press",
                        "publishedDate": "2009-07-31",
                        "description": "A comprehensive textbook.",
                        "industryIdentifiers": [
                            {"type": "ISBN_10", "identifier": "0262033844"},
                            {"type": "ISBN_13", "identifier": "9780262033848"}
                        ],
                        "printType": "BOOK",
                        "infoLink": "https://books.google.com/books?id=aefUBQAAQBAJ"
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = GoogleBooksProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ).with_base_url(server.uri()),
        };

        let result = provider.search("978-0-262-03384-8", SearchType::Isbn, 10, 0).await.unwrap();

        assert_eq!(result.total_hits, Some(1));
        let p = &result.papers[0];
        assert_eq!(p.title, "Introduction to Algorithms");
        assert_eq!(p.isbn.as_deref(), Some("9780262033848"));
        assert_eq!(p.publisher.as_deref(), Some("MIT Press"));
        assert_eq!(p.work_type.as_deref(), Some("book"));
        assert_eq!(p.abstract_text.as_deref(), Some("A comprehensive textbook."));
        assert_eq!(p.year, Some(2009));
        assert_eq!(p.source, "google_books");
    }

    #[tokio::test]
    async fn empty_results_no_items_key() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/books/v1/volumes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "totalItems": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = GoogleBooksProvider {
            base: ProviderBase::new(
                reqwest::Client::new(), Arc::new(Config::default()), Duration::from_millis(0),
            ).with_base_url(server.uri()),
        };

        let result = provider.search("xyznonexistent", SearchType::Keywords, 10, 0).await.unwrap();
        assert_eq!(result.total_hits, Some(0));
        assert!(result.papers.is_empty());
    }
}
