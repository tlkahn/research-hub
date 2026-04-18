use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

const FIELDS: &str =
    "title,authors,abstract,externalIds,year,url,openAccessPdf,journal,citationCount";

fn parse_paper(data: &serde_json::Value) -> Paper {
    let authors: Vec<String> = data
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let doi = data
        .get("externalIds")
        .and_then(|ids| ids.get("DOI"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let pdf_url = data
        .get("openAccessPdf")
        .and_then(|oa| oa.get("url"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let journal_info = data.get("journal");

    Paper {
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        authors,
        abstract_text: data
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(String::from),
        doi,
        year: data.get("year").and_then(|v| v.as_i64()).map(|y| y as i32),
        source: "semantic_scholar".into(),
        url: data.get("url").and_then(|v| v.as_str()).map(String::from),
        pdf_url,
        journal: journal_info
            .and_then(|j| j.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from),
        volume: journal_info
            .and_then(|j| j.get("volume"))
            .and_then(|v| v.as_str())
            .map(String::from),
        pages: journal_info
            .and_then(|j| j.get("pages"))
            .and_then(|v| v.as_str())
            .map(String::from),
        citation_count: data.get("citationCount").and_then(|v| v.as_i64()),
        ..Default::default()
    }
}

pub struct SemanticScholarProvider {
    base: ProviderBase,
}

impl SemanticScholarProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(200)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.semanticscholar.org/graph/v1")
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(key) = &self.base.config.semantic_scholar_api_key
            && let Ok(val) = reqwest::header::HeaderValue::from_str(key) {
                headers.insert("x-api-key", val);
            }
        headers
    }
}

#[async_trait]
impl Provider for SemanticScholarProvider {
    fn name(&self) -> &str {
        "semantic_scholar"
    }
    fn priority(&self) -> i32 {
        88
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[
            SearchType::Keywords,
            SearchType::Doi,
            SearchType::Author,
            SearchType::Title,
        ]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
    ) -> Result<ProviderResult> {
        let base = &self.base;
        retry("semantic_scholar", 3, || async {
            base.rate_limiter.wait().await;
            let headers = self.headers();

            if search_type == SearchType::Doi {
                let doi = query.trim_start_matches("https://doi.org/");
                let url = format!("{}/paper/DOI:{}", self.base_url(), doi);
                let resp = base
                    .client
                    .get(&url)
                    .headers(headers.clone())
                    .query(&[("fields", FIELDS)])
                    .send()
                    .await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(ProviderResult { papers: vec![], total_hits: None });
                }
                resp.error_for_status_ref()?;
                let data: serde_json::Value = resp.json().await?;
                return Ok(ProviderResult { papers: vec![parse_paper(&data)], total_hits: None });
            }

            let url = format!("{}/paper/search", self.base_url());
            let resp = base
                .client
                .get(&url)
                .headers(headers.clone())
                .query(&[
                    ("query", query),
                    ("limit", &limit.to_string()),
                    ("fields", FIELDS),
                ])
                .send()
                .await?;
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;
            let total_hits = data.get("total").and_then(|v| v.as_u64()).map(|n| n as usize);
            let papers_data = data
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let papers = papers_data.iter().take(limit).map(parse_paper).collect();
            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }

    async fn get_pdf_url(&self, doi: &str) -> Result<Option<String>> {
        let url = format!("{}/paper/DOI:{}", self.base_url(), doi);
        let resp = self
            .base
            .client
            .get(&url)
            .headers(self.headers())
            .query(&[("fields", "openAccessPdf")])
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r.json().await?;
                Ok(data
                    .get("openAccessPdf")
                    .and_then(|oa| oa.get("url"))
                    .and_then(|v| v.as_str())
                    .map(String::from))
            }
            _ => Ok(None),
        }
    }
}
