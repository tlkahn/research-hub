use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

pub struct ZenodoProvider {
    base: ProviderBase,
}

impl ZenodoProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(200)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://zenodo.org")
    }
}

fn build_query(query: &str, search_type: &SearchType) -> String {
    match search_type {
        SearchType::Keywords => query.to_string(),
        SearchType::Title => format!("title:\"{}\"", query),
        SearchType::Author => format!("creators.name:\"{}\"", query),
        SearchType::Doi => {
            let doi = query
                .strip_prefix("https://doi.org/")
                .or_else(|| query.strip_prefix("http://doi.org/"))
                .unwrap_or(query);
            format!("doi:\"{}\"", doi)
        }
        _ => String::new(),
    }
}

fn strip_html(html: &str) -> String {
    let fragment = scraper::Html::parse_fragment(html);
    fragment
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

fn parse_record(record: &serde_json::Value) -> Option<Paper> {
    let metadata = record.get("metadata")?;
    let title = metadata.get("title")?.as_str()?;
    if title.is_empty() {
        return None;
    }

    let authors = metadata
        .get("creators")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let doi = metadata
        .get("doi")
        .and_then(|v| v.as_str())
        .or_else(|| record.get("doi").and_then(|v| v.as_str()))
        .map(String::from);

    let pub_date = metadata
        .get("publication_date")
        .and_then(|v| v.as_str())
        .map(String::from);

    let year = pub_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let abstract_text = metadata
        .get("description")
        .and_then(|v| v.as_str())
        .map(strip_html)
        .filter(|s| !s.is_empty());

    let journal_obj = metadata.get("journal");
    let journal = journal_obj
        .and_then(|j| j.get("title"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let volume = journal_obj
        .and_then(|j| j.get("volume"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let issue = journal_obj
        .and_then(|j| j.get("issue"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let pages = journal_obj
        .and_then(|j| j.get("pages"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let url = doi
        .as_ref()
        .map(|d| format!("https://doi.org/{}", d))
        .or_else(|| {
            record
                .get("links")
                .and_then(|l| {
                    l.get("html")
                        .or_else(|| l.get("self_html"))
                })
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let pdf_url = record
        .get("files")
        .and_then(|f| f.as_array())
        .and_then(|files| {
            files.iter().find_map(|entry| {
                let key = entry.get("key")?.as_str()?;
                if key.ends_with(".pdf") {
                    entry
                        .get("links")
                        .and_then(|l| l.get("self"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                } else {
                    None
                }
            })
        });

    let work_type = metadata
        .get("resource_type")
        .and_then(|rt| rt.get("type"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let isbn = metadata
        .get("imprint")
        .and_then(|imp| imp.get("isbn"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let publisher = metadata
        .get("imprint")
        .and_then(|imp| imp.get("publisher"))
        .and_then(|v| v.as_str())
        .map(String::from);

    Some(Paper {
        title: title.to_string(),
        authors,
        doi,
        year,
        published_date: pub_date,
        abstract_text,
        journal,
        volume,
        issue,
        pages,
        url,
        pdf_url,
        work_type,
        isbn,
        publisher,
        source: "zenodo".to_string(),
        ..Default::default()
    })
}

#[async_trait]
impl Provider for ZenodoProvider {
    fn name(&self) -> &str {
        "zenodo"
    }

    fn priority(&self) -> i32 {
        50
    }

    fn base_delay(&self) -> Duration {
        Duration::from_millis(200)
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

        let q = build_query(query, &search_type);
        let page = (offset / limit.max(1)) + 1;

        let base_url = self.base_url().to_string();
        let url = format!("{}/api/records", base_url);

        let base = &self.base;
        retry("zenodo", 3, || async {
            base.rate_limiter.wait().await;

            let resp = base
                .client
                .get(&url)
                .query(&[
                    ("q", q.as_str()),
                    ("size", &limit.to_string()),
                    ("page", &page.to_string()),
                ])
                .header("Accept", "application/json")
                .send()
                .await?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(ProviderResult {
                    papers: vec![],
                    total_hits: None,
                });
            }
            resp.error_for_status_ref()?;

            let response: serde_json::Value = resp.json().await?;

            let total_hits = response["hits"]["total"]
                .as_u64()
                .or_else(|| response["hits"]["total"]["value"].as_u64())
                .map(|n| n as usize);

            let papers = response["hits"]["hits"]
                .as_array()
                .map(|hits| hits.iter().filter_map(parse_record).collect::<Vec<_>>())
                .unwrap_or_default();

            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_record_full_data() {
        let record = serde_json::json!({
            "metadata": {
                "title": "Machine Learning for Climate Science",
                "creators": [
                    {"name": "Smith, John"},
                    {"name": "Doe, Jane"}
                ],
                "doi": "10.5281/zenodo.1234567",
                "publication_date": "2024-03-15",
                "description": "<p>This paper explores <b>ML</b> techniques.</p>",
                "journal": {
                    "title": "Nature Climate Change",
                    "volume": "14",
                    "issue": "3",
                    "pages": "201-215"
                },
                "resource_type": {
                    "type": "publication"
                },
                "imprint": {
                    "isbn": "978-3-16-148410-0",
                    "publisher": "Zenodo Press"
                }
            },
            "doi": "10.5281/zenodo.1234567",
            "files": [
                {
                    "key": "paper.pdf",
                    "links": {
                        "self": "https://zenodo.org/api/files/bucket-id/paper.pdf"
                    }
                }
            ],
            "links": {
                "html": "https://zenodo.org/records/1234567"
            }
        });

        let paper = parse_record(&record).expect("should parse full record");
        assert_eq!(paper.title, "Machine Learning for Climate Science");
        assert_eq!(paper.authors, vec!["Smith, John", "Doe, Jane"]);
        assert_eq!(paper.doi, Some("10.5281/zenodo.1234567".to_string()));
        assert_eq!(paper.year, Some(2024));
        assert_eq!(paper.published_date, Some("2024-03-15".to_string()));
        assert!(paper.abstract_text.as_ref().unwrap().contains("ML techniques"));
        assert!(!paper.abstract_text.as_ref().unwrap().contains("<p>")); // HTML stripped
        assert_eq!(paper.journal, Some("Nature Climate Change".to_string()));
        assert_eq!(paper.volume, Some("14".to_string()));
        assert_eq!(paper.issue, Some("3".to_string()));
        assert_eq!(paper.pages, Some("201-215".to_string()));
        assert_eq!(paper.work_type, Some("publication".to_string()));
        assert_eq!(paper.isbn, Some("978-3-16-148410-0".to_string()));
        assert_eq!(paper.publisher, Some("Zenodo Press".to_string()));
        assert_eq!(
            paper.pdf_url,
            Some("https://zenodo.org/api/files/bucket-id/paper.pdf".to_string())
        );
        assert_eq!(paper.source, "zenodo");
    }

    #[test]
    fn test_parse_record_minimal_data() {
        let record = serde_json::json!({
            "metadata": {
                "title": "A Brief Note",
                "creators": [{"name": "Anonymous"}]
            }
        });

        let paper = parse_record(&record).expect("should parse minimal record");
        assert_eq!(paper.title, "A Brief Note");
        assert_eq!(paper.authors, vec!["Anonymous"]);
        assert_eq!(paper.doi, None);
        assert_eq!(paper.year, None);
        assert_eq!(paper.pdf_url, None);
        assert_eq!(paper.source, "zenodo");
    }

    #[test]
    fn test_parse_record_missing_title() {
        let record = serde_json::json!({
            "metadata": {
                "creators": [{"name": "Smith, John"}]
            }
        });
        assert!(parse_record(&record).is_none());
    }

    #[test]
    fn test_parse_record_year_only_date() {
        let record = serde_json::json!({
            "metadata": {
                "title": "Year Only",
                "creators": [],
                "publication_date": "2023"
            }
        });
        let paper = parse_record(&record).unwrap();
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.published_date, Some("2023".to_string()));
    }

    #[test]
    fn test_query_construction_keywords() {
        let q = build_query("machine learning", &SearchType::Keywords);
        assert_eq!(q, "machine learning");
    }

    #[test]
    fn test_query_construction_title() {
        let q = build_query("attention is all you need", &SearchType::Title);
        assert_eq!(q, "title:\"attention is all you need\"");
    }

    #[test]
    fn test_query_construction_author() {
        let q = build_query("Einstein", &SearchType::Author);
        assert_eq!(q, "creators.name:\"Einstein\"");
    }

    #[test]
    fn test_query_construction_doi() {
        // With URL prefix
        let q = build_query("https://doi.org/10.5281/zenodo.123", &SearchType::Doi);
        assert_eq!(q, "doi:\"10.5281/zenodo.123\"");

        // Without prefix
        let q = build_query("10.5281/zenodo.123", &SearchType::Doi);
        assert_eq!(q, "doi:\"10.5281/zenodo.123\"");
    }

    #[test]
    fn test_supported_search_types() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = ZenodoProvider::new(client, config);
        let types = provider.supported_search_types();
        assert!(types.contains(&SearchType::Keywords));
        assert!(types.contains(&SearchType::Title));
        assert!(types.contains(&SearchType::Author));
        assert!(types.contains(&SearchType::Doi));
        assert!(!types.contains(&SearchType::Isbn));
    }

    #[test]
    fn test_provider_metadata() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::default());
        let provider = ZenodoProvider::new(client, config);
        assert_eq!(provider.name(), "zenodo");
        assert_eq!(provider.priority(), 50);
        assert_eq!(provider.base_delay(), Duration::from_millis(200));
    }

    #[test]
    fn test_parse_record_pdf_from_files() {
        let record = serde_json::json!({
            "metadata": {
                "title": "Multi-file Record",
                "creators": [{"name": "Test Author"}]
            },
            "files": [
                {
                    "key": "data.csv",
                    "links": {"self": "https://zenodo.org/api/files/bucket/data.csv"}
                },
                {
                    "key": "manuscript.pdf",
                    "links": {"self": "https://zenodo.org/api/files/bucket/manuscript.pdf"}
                }
            ]
        });
        let paper = parse_record(&record).unwrap();
        assert_eq!(
            paper.pdf_url,
            Some("https://zenodo.org/api/files/bucket/manuscript.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_record_html_description() {
        let record = serde_json::json!({
            "metadata": {
                "title": "HTML Test",
                "creators": [],
                "description": "<p>First paragraph.</p><p>Second <em>emphasized</em> paragraph.</p>"
            }
        });
        let paper = parse_record(&record).unwrap();
        let abs = paper.abstract_text.unwrap();
        assert!(abs.contains("First paragraph."));
        assert!(abs.contains("Second emphasized paragraph."));
        assert!(!abs.contains("<p>"));
        assert!(!abs.contains("<em>"));
    }
}
