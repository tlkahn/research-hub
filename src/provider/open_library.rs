use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, extract_year, retry};

/// Pick the best ISBN from an array of ISBN values.
/// Prefers ISBN-13 (13 digits after stripping hyphens), falls back to the first entry.
fn pick_isbn(isbns: &[serde_json::Value]) -> Option<String> {
    let isbn13 = isbns
        .iter()
        .filter_map(|v| v.as_str())
        .find(|s| s.replace('-', "").len() == 13);
    if let Some(isbn) = isbn13 {
        return Some(isbn.to_string());
    }
    isbns.first().and_then(|v| v.as_str()).map(String::from)
}

/// Parse a search result document from the /search.json endpoint.
fn parse_doc(doc: &serde_json::Value) -> Paper {
    let title = doc
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let authors: Vec<String> = doc
        .get("author_name")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let year = doc
        .get("first_publish_year")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);

    let published_date = year.map(|y| format!("{y:04}-01-01"));

    let isbn = doc
        .get("isbn")
        .and_then(|v| v.as_array())
        .and_then(|arr| pick_isbn(arr));

    let publisher = doc
        .get("publisher")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let oclc = doc
        .get("oclc")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let lccn = doc
        .get("lccn")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let url = doc
        .get("key")
        .and_then(|v| v.as_str())
        .map(|key| format!("https://openlibrary.org{key}"));

    Paper {
        title,
        authors,
        year,
        published_date,
        isbn,
        publisher,
        oclc,
        lccn,
        url,
        source: "open_library".into(),
        ..Default::default()
    }
}

/// Parse a result from the /isbn/{isbn}.json endpoint.
/// This endpoint returns a different schema than the search endpoint.
fn parse_isbn_result(data: &serde_json::Value) -> Paper {
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let publish_date_str = data
        .get("publish_date")
        .and_then(|v| v.as_str());

    let year = publish_date_str.and_then(extract_year);

    let published_date = publish_date_str.map(String::from);

    let publisher = data
        .get("publishers")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    // Prefer isbn_13, fallback to isbn_10
    let isbn = data
        .get("isbn_13")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            data.get("isbn_10")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let oclc = data
        .get("oclcs")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let lccn = data
        .get("lccn")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let url = data
        .get("key")
        .and_then(|v| v.as_str())
        .map(|key| format!("https://openlibrary.org{key}"));

    Paper {
        title,
        authors: vec![],
        year,
        published_date,
        isbn,
        publisher,
        oclc,
        lccn,
        url,
        source: "open_library".into(),
        ..Default::default()
    }
}

pub struct OpenLibraryProvider {
    base: ProviderBase,
}

impl OpenLibraryProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(600)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://openlibrary.org")
    }
}

#[async_trait]
impl Provider for OpenLibraryProvider {
    fn name(&self) -> &str {
        "open_library"
    }
    fn priority(&self) -> i32 {
        60
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(600)
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

        let base = &self.base;

        if search_type == SearchType::Isbn {
            let isbn = query.replace('-', "");
            let url = format!("{}/isbn/{}.json", self.base_url(), isbn);

            return retry("open_library", 3, || {
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
                    let paper = parse_isbn_result(&data);
                    Ok(ProviderResult {
                        papers: vec![paper],
                        total_hits: Some(1),
                    })
                }
            })
            .await;
        }

        // Search endpoint for Keywords, Title, Author
        let param_name = match search_type {
            SearchType::Title => "title",
            SearchType::Author => "author",
            _ => "q",
        };

        let base_url = self.base_url().to_string();
        let query_owned = query.to_string();

        retry("open_library", 3, || {
            let base_url = base_url.clone();
            let query_owned = query_owned.clone();
            async move {
                base.rate_limiter.wait().await;
                let url = format!("{}/search.json", base_url);
                let resp = base
                    .client
                    .get(&url)
                    .query(&[
                        (param_name, query_owned.as_str()),
                        ("offset", &offset.to_string()),
                        ("limit", &limit.to_string()),
                    ])
                    .send()
                    .await?;
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;
                let total_hits = data
                    .get("numFound")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let papers = data
                    .get("docs")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().map(parse_doc).collect())
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

    // ---- parse_doc tests ----

    #[test]
    fn test_parse_doc_full() {
        let doc = serde_json::json!({
            "title": "Introduction to Algorithms",
            "author_name": ["Thomas H. Cormen", "Charles E. Leiserson"],
            "first_publish_year": 1990,
            "isbn": ["0262031418", "9780262033848"],
            "publisher": ["MIT Press", "McGraw-Hill"],
            "oclc": ["21442839"],
            "lccn": ["89013027"],
            "key": "/works/OL3295713W"
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.title, "Introduction to Algorithms");
        assert_eq!(
            paper.authors,
            vec!["Thomas H. Cormen", "Charles E. Leiserson"]
        );
        assert_eq!(paper.year, Some(1990));
        assert_eq!(paper.published_date, Some("1990-01-01".to_string()));
        // ISBN-13 should be preferred
        assert_eq!(paper.isbn, Some("9780262033848".to_string()));
        assert_eq!(paper.publisher, Some("MIT Press".to_string()));
        assert_eq!(paper.oclc, Some("21442839".to_string()));
        assert_eq!(paper.lccn, Some("89013027".to_string()));
        assert_eq!(
            paper.url,
            Some("https://openlibrary.org/works/OL3295713W".to_string())
        );
        assert_eq!(paper.source, "open_library");
        assert_eq!(paper.doi, None);
        assert_eq!(paper.pdf_url, None);
        assert_eq!(paper.journal, None);
        assert_eq!(paper.volume, None);
        assert_eq!(paper.issue, None);
        assert_eq!(paper.pages, None);
        assert_eq!(paper.citation_count, None);
        assert_eq!(paper.abstract_text, None);
    }

    #[test]
    fn test_parse_doc_minimal() {
        let doc = serde_json::json!({
            "title": "Minimal Book"
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.title, "Minimal Book");
        assert!(paper.authors.is_empty());
        assert_eq!(paper.doi, None);
        assert_eq!(paper.year, None);
        assert_eq!(paper.published_date, None);
        assert_eq!(paper.isbn, None);
        assert_eq!(paper.publisher, None);
        assert_eq!(paper.oclc, None);
        assert_eq!(paper.lccn, None);
        assert_eq!(paper.url, None);
        assert_eq!(paper.source, "open_library");
    }

    #[test]
    fn test_parse_doc_isbn_prefers_isbn13() {
        let doc = serde_json::json!({
            "title": "Test Book",
            "isbn": ["1234567890", "9781234567890"]
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.isbn, Some("9781234567890".to_string()));
    }

    #[test]
    fn test_parse_doc_isbn_fallback_to_first() {
        let doc = serde_json::json!({
            "title": "Test Book",
            "isbn": ["1234567890", "0987654321"]
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.isbn, Some("1234567890".to_string()));
    }

    #[test]
    fn test_parse_doc_url_from_key() {
        let doc = serde_json::json!({
            "title": "URL Test",
            "key": "/works/OL12345W"
        });
        let paper = parse_doc(&doc);
        assert_eq!(
            paper.url,
            Some("https://openlibrary.org/works/OL12345W".to_string())
        );
    }

    #[test]
    fn test_parse_doc_oclc_and_lccn() {
        let doc = serde_json::json!({
            "title": "Identifiers Test",
            "oclc": ["12345"],
            "lccn": ["2023456789"]
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.oclc, Some("12345".to_string()));
        assert_eq!(paper.lccn, Some("2023456789".to_string()));
    }

    #[test]
    fn test_parse_doc_year_zero_padded() {
        let doc = serde_json::json!({
            "title": "Ancient Text",
            "first_publish_year": 85
        });
        let paper = parse_doc(&doc);
        assert_eq!(paper.year, Some(85));
        assert_eq!(
            paper.published_date,
            Some("0085-01-01".to_string()),
            "Years below 1000 must be zero-padded to 4 digits"
        );
    }

    // ---- parse_isbn_result tests ----

    #[test]
    fn test_parse_isbn_result_full() {
        let data = serde_json::json!({
            "title": "The Art of Computer Programming",
            "publishers": ["Addison-Wesley"],
            "publish_date": "January 1, 1997",
            "isbn_13": ["9780201896831"],
            "isbn_10": ["0201896834"],
            "oclcs": ["34669828"],
            "lccn": ["97002147"],
            "key": "/books/OL1234M"
        });
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.title, "The Art of Computer Programming");
        assert_eq!(paper.publisher, Some("Addison-Wesley".to_string()));
        assert_eq!(paper.isbn, Some("9780201896831".to_string()));
        assert_eq!(paper.oclc, Some("34669828".to_string()));
        assert_eq!(paper.lccn, Some("97002147".to_string()));
        assert_eq!(paper.year, Some(1997));
        assert_eq!(
            paper.url,
            Some("https://openlibrary.org/books/OL1234M".to_string())
        );
        assert_eq!(paper.source, "open_library");
    }

    #[test]
    fn test_parse_isbn_result_minimal() {
        let data = serde_json::json!({
            "title": "Bare Minimum"
        });
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.title, "Bare Minimum");
        assert!(paper.authors.is_empty());
        assert_eq!(paper.publisher, None);
        assert_eq!(paper.isbn, None);
        assert_eq!(paper.year, None);
        assert_eq!(paper.source, "open_library");
    }

    #[test]
    fn test_parse_isbn_result_authors_empty() {
        let data = serde_json::json!({
            "title": "No Authors",
            "publishers": ["Test Publisher"]
        });
        let paper = parse_isbn_result(&data);
        assert!(
            paper.authors.is_empty(),
            "ISBN endpoint does not return authors"
        );
    }

    #[test]
    fn test_parse_isbn_result_isbn13_preferred() {
        let data = serde_json::json!({
            "title": "ISBN Preference",
            "isbn_13": ["9781234567890"],
            "isbn_10": ["1234567890"]
        });
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.isbn, Some("9781234567890".to_string()));
    }

    #[test]
    fn test_parse_isbn_result_isbn10_fallback() {
        let data = serde_json::json!({
            "title": "ISBN-10 Only",
            "isbn_10": ["1234567890"]
        });
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.isbn, Some("1234567890".to_string()));
    }

    // ---- pick_isbn tests ----

    #[test]
    fn test_pick_isbn_empty() {
        let isbns: Vec<serde_json::Value> = vec![];
        assert_eq!(pick_isbn(&isbns), None);
    }

    #[test]
    fn test_pick_isbn_prefers_isbn13() {
        let isbns = vec![
            serde_json::json!("1234567890"),
            serde_json::json!("9781234567890"),
        ];
        assert_eq!(pick_isbn(&isbns), Some("9781234567890".to_string()));
    }

    #[test]
    fn test_pick_isbn_fallback_to_first() {
        let isbns = vec![
            serde_json::json!("1234567890"),
            serde_json::json!("0987654321"),
        ];
        assert_eq!(pick_isbn(&isbns), Some("1234567890".to_string()));
    }

    // ---- provider metadata tests ----

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = OpenLibraryProvider::new(client, config);
        assert_eq!(provider.name(), "open_library");
        assert_eq!(provider.priority(), 60);
        assert_eq!(provider.base_delay(), Duration::from_millis(600));
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = OpenLibraryProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Keywords));
        assert!(types.contains(&SearchType::Title));
        assert!(types.contains(&SearchType::Author));
        assert!(types.contains(&SearchType::Isbn));
        assert!(!types.contains(&SearchType::Doi));
    }

    // ---- parse_doc missing title defaults ----

    #[test]
    fn test_parse_doc_missing_title_defaults_to_unknown() {
        let doc = serde_json::json!({});
        let paper = parse_doc(&doc);
        assert_eq!(paper.title, "Unknown");
    }

    // ---- parse_isbn_result missing title ----

    #[test]
    fn test_parse_isbn_result_missing_title_defaults_to_unknown() {
        let data = serde_json::json!({});
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.title, "Unknown");
    }

    // ---- parse_isbn_result publish_date stored ----

    #[test]
    fn test_parse_isbn_result_publish_date_stored() {
        let data = serde_json::json!({
            "title": "Date Test",
            "publish_date": "March 2020"
        });
        let paper = parse_isbn_result(&data);
        assert_eq!(paper.published_date, Some("March 2020".to_string()));
        assert_eq!(paper.year, Some(2020));
    }
}
