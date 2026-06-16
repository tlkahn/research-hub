use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

/// Characters that must be percent-encoded when placed in a URL path segment.
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'#')
    .add(b'?')
    .add(b'%')
    .add(b'[')
    .add(b']');

fn build_query(query: &str, search_type: SearchType) -> String {
    match search_type {
        SearchType::Doi => {
            let doi = query
                .strip_prefix("https://doi.org/")
                .or_else(|| query.strip_prefix("http://doi.org/"))
                .unwrap_or(query);
            format!("doi:{doi}")
        }
        SearchType::Title => format!("title:{query}"),
        SearchType::Author => format!("author:{query}"),
        SearchType::Keywords => query.to_string(),
        _ => query.to_string(),
    }
}

fn parse_article(article: &serde_json::Value) -> Paper {
    let bibjson = article.get("bibjson").unwrap_or(article);

    let title = bibjson
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let authors = bibjson
        .get("author")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let abstract_text = bibjson
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.is_empty());

    // DOI: find identifier with type "doi"
    let doi = bibjson
        .get("identifier")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|id| {
                let id_type = id.get("type").and_then(|v| v.as_str())?;
                if id_type == "doi" {
                    id.get("id").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
        })
        .map(|d| {
            d.strip_prefix("https://doi.org/")
                .or_else(|| d.strip_prefix("http://doi.org/"))
                .unwrap_or(&d)
                .to_string()
        })
        .filter(|s| !s.is_empty());

    // ISSN: find identifier with type "eissn" or "pissn"
    let issn = bibjson
        .get("identifier")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|id| {
                let id_type = id.get("type").and_then(|v| v.as_str())?;
                if id_type == "eissn" || id_type == "pissn" {
                    id.get("id").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
        });

    // Journal metadata
    let journal_obj = bibjson.get("journal");
    let journal = journal_obj
        .and_then(|j| j.get("title"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let volume = journal_obj
        .and_then(|j| j.get("volume"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let issue = journal_obj
        .and_then(|j| j.get("number"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let publisher = journal_obj
        .and_then(|j| j.get("publisher"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Pages: start_page - end_page
    let start_page = bibjson.get("start_page").and_then(|v| v.as_str());
    let end_page = bibjson.get("end_page").and_then(|v| v.as_str());
    let pages = match (start_page, end_page) {
        (Some(s), Some(e)) if !s.is_empty() && !e.is_empty() => Some(format!("{s}-{e}")),
        (Some(s), _) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };

    // Year (string in DOAJ API)
    let year = bibjson
        .get("year")
        .and_then(|v| v.as_str())
        .and_then(|y| y.parse::<i32>().ok());

    // Published date from year + optional month
    let month = bibjson
        .get("month")
        .and_then(|v| v.as_str())
        .and_then(|m| m.parse::<u32>().ok());
    let published_date = year.map(|y| {
        if let Some(m) = month {
            format!("{:04}-{:02}-01", y, m)
        } else {
            format!("{:04}-01-01", y)
        }
    });

    // URL: first link's url, or construct from DOI
    let url = bibjson
        .get("link")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|link| link.get("url").and_then(|v| v.as_str()))
        .map(String::from)
        .or_else(|| doi.as_ref().map(|d| format!("https://doi.org/{d}")));

    Paper {
        title,
        authors,
        abstract_text,
        doi,
        year,
        published_date,
        source: "doaj".into(),
        url,
        pdf_url: None,
        journal,
        volume,
        issue,
        pages,
        publisher,
        issn,
        work_type: Some("journal-article".into()),
        ..Default::default()
    }
}

pub struct DoajProvider {
    base: ProviderBase,
}

impl DoajProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(500)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://doaj.org")
    }
}

#[async_trait]
impl Provider for DoajProvider {
    fn name(&self) -> &str {
        "doaj"
    }
    fn priority(&self) -> i32 {
        45
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[
            SearchType::Keywords,
            SearchType::Title,
            SearchType::Author,
            SearchType::Doi,
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
        retry("doaj", 3, || async {
            base.rate_limiter.wait().await;

            let q = build_query(query, search_type);
            let encoded = utf8_percent_encode(&q, PATH_SEGMENT_ENCODE_SET).to_string();
            let page = (offset / limit.max(1)) + 1;

            let url = format!("{}/api/search/articles/{}", self.base_url(), encoded);
            let resp = base
                .client
                .get(&url)
                .query(&[
                    ("page", page.to_string()),
                    ("pageSize", limit.to_string()),
                ])
                .send()
                .await?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(ProviderResult {
                    papers: vec![],
                    total_hits: None,
                });
            }
            resp.error_for_status_ref()?;

            let data: serde_json::Value = resp.json().await?;
            let total_hits = data
                .get("total")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let results = data
                .get("results")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let papers = results.iter().take(limit).map(parse_article).collect();

            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_article tests ---

    #[test]
    fn test_parse_article_full() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "Open Access and Machine Learning",
                "author": [
                    {"name": "Alice Smith"},
                    {"name": "Bob Jones"}
                ],
                "abstract": "This paper explores open access publishing.",
                "identifier": [
                    {"type": "doi", "id": "10.1234/doaj.5678"},
                    {"type": "eissn", "id": "1234-5678"}
                ],
                "journal": {
                    "title": "Journal of Open Science",
                    "volume": "12",
                    "number": "3",
                    "publisher": "Open Access Publishers"
                },
                "start_page": "100",
                "end_page": "115",
                "year": "2023",
                "month": "6",
                "link": [
                    {"url": "https://example.com/article/123"}
                ]
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.title, "Open Access and Machine Learning");
        assert_eq!(paper.authors, vec!["Alice Smith", "Bob Jones"]);
        assert_eq!(
            paper.abstract_text,
            Some("This paper explores open access publishing.".to_string())
        );
        assert_eq!(paper.doi, Some("10.1234/doaj.5678".to_string()));
        assert_eq!(paper.issn, Some("1234-5678".to_string()));
        assert_eq!(paper.journal, Some("Journal of Open Science".to_string()));
        assert_eq!(paper.volume, Some("12".to_string()));
        assert_eq!(paper.issue, Some("3".to_string()));
        assert_eq!(paper.publisher, Some("Open Access Publishers".to_string()));
        assert_eq!(paper.pages, Some("100-115".to_string()));
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023-06-01".to_string()));
        assert_eq!(paper.url, Some("https://example.com/article/123".to_string()));
        assert_eq!(paper.source, "doaj");
        assert_eq!(paper.work_type, Some("journal-article".to_string()));
        assert!(paper.pdf_url.is_none());
    }

    #[test]
    fn test_parse_article_minimal() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "A Minimal Article"
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.title, "A Minimal Article");
        assert!(paper.authors.is_empty());
        assert!(paper.doi.is_none());
        assert!(paper.abstract_text.is_none());
        assert_eq!(paper.source, "doaj");
        assert_eq!(paper.work_type, Some("journal-article".to_string()));
    }

    #[test]
    fn test_parse_article_doi_extraction() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "DOI Test",
                "identifier": [
                    {"type": "eissn", "id": "9999-0000"},
                    {"type": "doi", "id": "10.5555/test.doi"}
                ]
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.doi, Some("10.5555/test.doi".to_string()));
        assert_eq!(paper.issn, Some("9999-0000".to_string()));
    }

    #[test]
    fn test_parse_article_doi_strip_url_prefix() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "DOI URL Prefix Test",
                "identifier": [
                    {"type": "doi", "id": "https://doi.org/10.1234/test"}
                ]
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.doi, Some("10.1234/test".to_string()));
    }

    #[test]
    fn test_parse_article_pages_both() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "Pages Test",
                "start_page": "42",
                "end_page": "55"
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.pages, Some("42-55".to_string()));
    }

    #[test]
    fn test_parse_article_pages_start_only() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "Start Page Only",
                "start_page": "42"
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.pages, Some("42".to_string()));
    }

    #[test]
    fn test_parse_article_no_pages() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "No Pages"
            }
        });

        let paper = parse_article(&article);
        assert!(paper.pages.is_none());
    }

    #[test]
    fn test_parse_article_published_date_year_and_month() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "Date Test",
                "year": "2023",
                "month": "6"
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023-06-01".to_string()));
    }

    #[test]
    fn test_parse_article_published_date_year_only() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "Year Only Test",
                "year": "2023"
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023-01-01".to_string()));
    }

    #[test]
    fn test_parse_article_url_fallback_to_doi() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "URL Fallback",
                "identifier": [
                    {"type": "doi", "id": "10.1234/fallback"}
                ]
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.url, Some("https://doi.org/10.1234/fallback".to_string()));
    }

    // --- build_query tests ---

    #[test]
    fn test_build_query_keywords() {
        let q = build_query("machine learning", SearchType::Keywords);
        assert_eq!(q, "machine learning");
    }

    #[test]
    fn test_build_query_title() {
        let q = build_query("attention is all you need", SearchType::Title);
        assert_eq!(q, "title:attention is all you need");
    }

    #[test]
    fn test_build_query_author() {
        let q = build_query("Einstein", SearchType::Author);
        assert_eq!(q, "author:Einstein");
    }

    #[test]
    fn test_build_query_doi() {
        let q = build_query("https://doi.org/10.1234/test", SearchType::Doi);
        assert_eq!(q, "doi:10.1234/test");
    }

    #[test]
    fn test_build_query_doi_without_prefix() {
        let q = build_query("10.1234/test", SearchType::Doi);
        assert_eq!(q, "doi:10.1234/test");
    }

    #[test]
    fn test_build_query_doi_http_prefix() {
        let q = build_query("http://doi.org/10.1234/test", SearchType::Doi);
        assert_eq!(q, "doi:10.1234/test");
    }

    #[test]
    fn test_parse_article_doi_strip_http_url_prefix() {
        let article = serde_json::json!({
            "bibjson": {
                "title": "DOI HTTP Prefix Test",
                "identifier": [
                    {"type": "doi", "id": "http://doi.org/10.1234/http-test"}
                ]
            }
        });

        let paper = parse_article(&article);
        assert_eq!(paper.doi, Some("10.1234/http-test".to_string()));
    }

    // --- provider metadata tests ---

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = DoajProvider::new(client, config);
        assert_eq!(provider.name(), "doaj");
        assert_eq!(provider.priority(), 45);
        assert_eq!(provider.base_delay(), Duration::from_millis(500));
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = DoajProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Keywords));
        assert!(types.contains(&SearchType::Title));
        assert!(types.contains(&SearchType::Author));
        assert!(types.contains(&SearchType::Doi));
        assert!(!types.contains(&SearchType::Isbn));
    }

    // --- URL encoding tests ---

    #[test]
    fn test_url_encoding_special_chars() {
        let query = "machine learning? or #deep";
        let encoded = utf8_percent_encode(query, PATH_SEGMENT_ENCODE_SET).to_string();
        assert!(!encoded.contains(' '));
        assert!(!encoded.contains('?'));
        assert!(!encoded.contains('#'));
        assert!(encoded.contains("%20")); // space
        assert!(encoded.contains("%3F")); // ?
        assert!(encoded.contains("%23")); // #
    }

    #[test]
    fn test_url_encoding_preserves_alphanumeric() {
        let query = "transformers2024";
        let encoded = utf8_percent_encode(query, PATH_SEGMENT_ENCODE_SET).to_string();
        assert_eq!(encoded, "transformers2024");
    }

    #[test]
    fn test_url_encoding_preserves_colons_and_slashes() {
        // Colons and slashes are valid in path segments and used in DOAJ query syntax
        let query = "doi:10.1234/test";
        let encoded = utf8_percent_encode(query, PATH_SEGMENT_ENCODE_SET).to_string();
        assert_eq!(encoded, "doi:10.1234/test");
    }
}
