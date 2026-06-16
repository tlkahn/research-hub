use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

fn strip_jats(text: &str) -> String {
    if text.starts_with("<jats:") || text.starts_with("<jats") {
        let fragment = scraper::Html::parse_fragment(text);
        fragment.root_element().text().collect::<String>()
    } else {
        text.to_string()
    }
}

fn find_pdf_link(item: &serde_json::Value) -> Option<String> {
    item.get("link")?
        .as_array()?
        .iter()
        .find(|link| {
            link.get("content-type")
                .and_then(|v| v.as_str())
                .is_some_and(|ct| ct == "application/pdf")
        })
        .and_then(|link| link.get("URL"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Returns true if the CrossRef work type indicates a book-like item where
/// `container-title[0]` typically represents a series or book name rather
/// than a journal name.
fn is_book_like_type(work_type: Option<&str>) -> bool {
    matches!(
        work_type,
        Some("book-chapter" | "book" | "monograph" | "book-part" | "book-section" | "edited-book")
    )
}

fn contributor_name(person: &serde_json::Value) -> Option<String> {
    let given = person.get("given").and_then(|v| v.as_str()).unwrap_or("");
    let family = person.get("family").and_then(|v| v.as_str()).unwrap_or("");
    let name = format!("{given} {family}").trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn parse_item(item: &serde_json::Value) -> Paper {
    let authors: Vec<String> = item
        .get("author")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(contributor_name).collect())
        .unwrap_or_default();

    let doi = item
        .get("DOI")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches("https://doi.org/").to_string())
        .filter(|s| !s.is_empty());

    let mut year = None;
    let mut published_date = None;
    for field in &["published-print", "published-online", "created"] {
        if let Some(parts) = item
            .get(field)
            .and_then(|v| v.get("date-parts"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_array())
            && let Some(y) = parts.first().and_then(|v| v.as_i64()) {
                year = Some(y as i32);
                let m = parts.get(1).and_then(|v| v.as_i64()).unwrap_or(1);
                let d = parts.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                published_date = Some(format!("{:04}-{:02}-{:02}", y, m, d));
                break;
            }
    }

    let title = item
        .get("title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let abstract_text = item
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(strip_jats);

    let journal = item
        .get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let url = doi
        .as_ref()
        .map(|d| format!("https://doi.org/{d}"));

    let work_type = item.get("type").and_then(|v| v.as_str()).map(String::from);

    let series = item.get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(1))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Post-hoc correction: when the work type is book-like and container-title
    // has only one element, that element is more likely a series/book name than
    // a journal name. Move it from journal to series.
    // See: https://api.crossref.org/types for the full list of CrossRef types.
    let container_title_len = item.get("container-title")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);

    let (journal, series) = if is_book_like_type(work_type.as_deref())
        && series.is_none()
        && journal.is_some()
        && container_title_len == 1
    {
        (None, journal)
    } else {
        (journal, series)
    };

    Paper {
        title,
        authors,
        abstract_text,
        doi: doi.clone(),
        year,
        published_date,
        source: "crossref".into(),
        url,
        pdf_url: find_pdf_link(item),
        journal,
        volume: item.get("volume").and_then(|v| v.as_str()).map(String::from),
        issue: item.get("issue").and_then(|v| v.as_str()).map(String::from),
        pages: item.get("page").and_then(|v| v.as_str()).map(String::from),
        publisher: item.get("publisher").and_then(|v| v.as_str()).map(String::from),
        isbn: item.get("ISBN")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(String::from),
        issn: item.get("ISSN")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(String::from),
        work_type,
        editors: item.get("editor")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(contributor_name).collect())
            .unwrap_or_default(),
        series,
        ..Default::default()
    }
}

pub struct CrossrefProvider {
    base: ProviderBase,
}

impl CrossrefProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(50)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.crossref.org")
    }

    fn mailto_params(&self) -> Vec<(&str, String)> {
        let mut params = Vec::new();
        if let Some(email) = &self.base.config.crossref_email {
            params.push(("mailto", email.clone()));
        }
        params
    }
}

#[async_trait]
impl Provider for CrossrefProvider {
    fn name(&self) -> &str {
        "crossref"
    }
    fn priority(&self) -> i32 {
        90
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(50)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[
            SearchType::Keywords,
            SearchType::Doi,
            SearchType::Author,
            SearchType::Title,
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
        let base = &self.base;
        retry("crossref", 3, || async {
            base.rate_limiter.wait().await;

            if search_type == SearchType::Doi {
                let doi = query.trim_start_matches("https://doi.org/");
                let url = format!("{}/works/{}", self.base_url(), doi);
                let resp = base
                    .client
                    .get(&url)
                    .query(&self.mailto_params())
                    .send()
                    .await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(ProviderResult { papers: vec![], total_hits: None });
                }
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;
                let item = data.get("message").cloned().unwrap_or_default();
                return Ok(ProviderResult { papers: vec![parse_item(&item)], total_hits: None });
            }

            let mut params = self.mailto_params();
            params.push(("rows", limit.to_string()));
            params.push(("offset", offset.to_string()));
            match search_type {
                SearchType::Author => params.push(("query.author", query.to_string())),
                SearchType::Title => params.push(("query.title", query.to_string())),
                SearchType::Isbn => {
                    let isbn: String = query.chars().filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x').collect();
                    params.push(("filter", format!("isbn:{isbn}")));
                }
                _ => params.push(("query", query.to_string())),
            }

            let url = format!("{}/works", self.base_url());
            let resp = base.client.get(&url).query(&params).send().await?;
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;
            let message = data.get("message");
            let total_hits = message
                .and_then(|m| m.get("total-results"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let items = message
                .and_then(|m| m.get("items"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let papers = items.iter().take(limit).map(parse_item).collect();
            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }

    async fn get_pdf_url(&self, doi: &str) -> Result<Option<String>> {
        let url = format!("{}/works/{}", self.base_url(), doi);
        let resp = self
            .base
            .client
            .get(&url)
            .query(&self.mailto_params())
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r.json().await?;
                Ok(find_pdf_link(
                    data.get("message").unwrap_or(&serde_json::Value::Null),
                ))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Book-chapter with single container-title: should land in `series`, not `journal`.
    #[test]
    fn test_parse_item_book_chapter_single_container_title() {
        let item = serde_json::json!({
            "type": "book-chapter",
            "title": ["A Chapter on Transformers"],
            "author": [{"given": "Jane", "family": "Doe"}],
            "DOI": "10.1007/978-3-030-12345-6_1",
            "container-title": ["Lecture Notes in Computer Science"],
            "publisher": "Springer",
            "ISBN": ["978-3-030-12345-6"],
            "published-print": {"date-parts": [[2023, 6, 15]]}
        });
        let paper = parse_item(&item);
        assert_eq!(paper.work_type.as_deref(), Some("book-chapter"));
        // Single container-title for a book-chapter should go to series, not journal
        assert_eq!(paper.series.as_deref(), Some("Lecture Notes in Computer Science"));
        assert!(paper.journal.is_none(), "journal should be None for book-chapter with single container-title");
    }

    /// Book-chapter with two container-title elements: [0] stays journal, [1] stays series.
    #[test]
    fn test_parse_item_book_chapter_two_container_titles() {
        let item = serde_json::json!({
            "type": "book-chapter",
            "title": ["A Chapter"],
            "author": [{"given": "John", "family": "Smith"}],
            "DOI": "10.1007/978-3-030-99999-9_2",
            "container-title": ["Advances in Neural Information Processing", "LNCS"],
            "publisher": "Springer",
            "published-print": {"date-parts": [[2022]]}
        });
        let paper = parse_item(&item);
        assert_eq!(paper.journal.as_deref(), Some("Advances in Neural Information Processing"));
        assert_eq!(paper.series.as_deref(), Some("LNCS"));
    }

    /// Monograph with single container-title: should go to series.
    #[test]
    fn test_parse_item_monograph_single_container_title() {
        let item = serde_json::json!({
            "type": "monograph",
            "title": ["Some Monograph"],
            "author": [],
            "DOI": "10.1234/mono.001",
            "container-title": ["Series of Important Monographs"]
        });
        let paper = parse_item(&item);
        assert_eq!(paper.series.as_deref(), Some("Series of Important Monographs"));
        assert!(paper.journal.is_none());
    }

    /// Regular journal-article: container-title[0] stays in journal, no swap.
    #[test]
    fn test_parse_item_journal_article_no_swap() {
        let item = serde_json::json!({
            "type": "journal-article",
            "title": ["Attention Is All You Need"],
            "author": [{"given": "Ashish", "family": "Vaswani"}],
            "DOI": "10.5555/3295222.3295349",
            "container-title": ["Advances in Neural Information Processing Systems"],
            "published-print": {"date-parts": [[2017]]}
        });
        let paper = parse_item(&item);
        assert_eq!(paper.journal.as_deref(), Some("Advances in Neural Information Processing Systems"));
        assert!(paper.series.is_none());
    }

    /// No work_type at all: container-title[0] stays in journal (safe default).
    #[test]
    fn test_parse_item_no_work_type_no_swap() {
        let item = serde_json::json!({
            "title": ["Some Paper"],
            "author": [],
            "DOI": "10.1234/notype",
            "container-title": ["Some Journal"]
        });
        let paper = parse_item(&item);
        assert_eq!(paper.journal.as_deref(), Some("Some Journal"));
        assert!(paper.series.is_none());
    }

    /// book type with single container-title: should swap to series.
    #[test]
    fn test_parse_item_book_single_container_title() {
        let item = serde_json::json!({
            "type": "book",
            "title": ["My Book"],
            "author": [{"given": "Alice", "family": "Writer"}],
            "DOI": "10.1234/book.001",
            "container-title": ["Oxford Studies in Philosophy"]
        });
        let paper = parse_item(&item);
        assert_eq!(paper.series.as_deref(), Some("Oxford Studies in Philosophy"));
        assert!(paper.journal.is_none());
    }

    /// book-chapter with NO container-title: both journal and series are None, no panic.
    #[test]
    fn test_parse_item_book_chapter_no_container_title() {
        let item = serde_json::json!({
            "type": "book-chapter",
            "title": ["Standalone Chapter"],
            "author": [],
            "DOI": "10.1234/standalone"
        });
        let paper = parse_item(&item);
        assert!(paper.journal.is_none());
        assert!(paper.series.is_none());
    }

    #[test]
    fn test_contributor_name_given_and_family() {
        let person = serde_json::json!({"given": "Jane", "family": "Doe"});
        assert_eq!(contributor_name(&person), Some("Jane Doe".into()));
    }

    #[test]
    fn test_contributor_name_family_only() {
        let person = serde_json::json!({"family": "Doe"});
        assert_eq!(contributor_name(&person), Some("Doe".into()));
    }

    #[test]
    fn test_contributor_name_given_only() {
        let person = serde_json::json!({"given": "Jane"});
        assert_eq!(contributor_name(&person), Some("Jane".into()));
    }

    #[test]
    fn test_contributor_name_both_empty() {
        let person = serde_json::json!({"given": "", "family": ""});
        assert_eq!(contributor_name(&person), None);
    }

    #[test]
    fn test_contributor_name_missing_fields() {
        let person = serde_json::json!({});
        assert_eq!(contributor_name(&person), None);
    }

    #[test]
    fn test_contributor_name_whitespace_only() {
        let person = serde_json::json!({"given": "  ", "family": "  "});
        assert_eq!(contributor_name(&person), None);
    }

    #[tokio::test]
    async fn isbn_search_strips_spaces_for_filter() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({
            "status": "ok",
            "message": {
                "total-results": 1,
                "items": [{
                    "type": "book",
                    "title": ["The Yoga of Power"],
                    "author": [{"given": "Julius", "family": "Evola"}],
                    "DOI": "10.7312/evol92241",
                    "ISBN": ["9780231179249"],
                    "publisher": "Inner Traditions",
                    "published-print": {"date-parts": [[1992]]}
                }]
            }
        });

        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("filter", "isbn:9780231179249"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let config = Arc::new(Config::from_env());
        let client = reqwest::Client::new();
        let provider = CrossrefProvider {
            base: ProviderBase::new(client, config, Duration::from_millis(0))
                .with_base_url(server.uri()),
        };

        let result = provider
            .search("978 023117924 9", SearchType::Isbn, 10, 0)
            .await
            .expect("search should succeed");

        assert_eq!(result.papers.len(), 1);
        assert_eq!(result.papers[0].title, "The Yoga of Power");
    }
}
