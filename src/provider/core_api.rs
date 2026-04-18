use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

fn parse_work(work: &serde_json::Value) -> Paper {
    let mut authors = Vec::new();
    if let Some(arr) = work.get("authors").and_then(|v| v.as_array()) {
        for a in arr {
            if let Some(name) = a.as_object().and_then(|o| o.get("name")).and_then(|v| v.as_str()) {
                authors.push(name.to_string());
            } else if let Some(name) = a.as_str() {
                authors.push(name.to_string());
            }
        }
    }

    let doi = work
        .get("doi")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches("https://doi.org/").to_string())
        .filter(|s| !s.is_empty());

    let year = work
        .get("yearPublished")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);

    let pdf_url = work
        .get("downloadUrl")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            work.get("sourceFulltextUrls")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let url = work
        .get("id")
        .and_then(|v| v.as_i64().map(|id| id.to_string()).or_else(|| v.as_str().map(String::from)))
        .map(|id| format!("https://core.ac.uk/works/{id}"));

    Paper {
        title: work
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        authors,
        abstract_text: work
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(String::from),
        doi,
        year,
        source: "core".into(),
        url,
        pdf_url,
        journal: work
            .get("publisher")
            .and_then(|v| v.as_str())
            .map(String::from),
        ..Default::default()
    }
}

pub struct CoreProvider {
    base: ProviderBase,
}

impl CoreProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(200)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.core.ac.uk/v3")
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(key) = &self.base.config.core_api_key
            && let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        headers
    }
}

#[async_trait]
impl Provider for CoreProvider {
    fn name(&self) -> &str {
        "core"
    }
    fn priority(&self) -> i32 {
        86
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Keywords, SearchType::Doi, SearchType::Title]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
    ) -> Result<ProviderResult> {
        let base = &self.base;
        retry("core", 3, || async {
            base.rate_limiter.wait().await;
            let headers = self.auth_headers();

            let q = match search_type {
                SearchType::Doi => {
                    let doi = query.trim_start_matches("https://doi.org/");
                    format!("doi:{doi}")
                }
                SearchType::Title => format!("title:({query})"),
                _ => query.to_string(),
            };

            let url = format!("{}/search/works", self.base_url());
            let resp = base
                .client
                .get(&url)
                .headers(headers.clone())
                .query(&[("q", &q), ("limit", &limit.to_string())])
                .send()
                .await?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND
                || resp.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                return Ok(ProviderResult { papers: vec![], total_hits: None });
            }
            resp.error_for_status_ref()?;

            let data: serde_json::Value = resp.json().await?;
            let total_hits = data.get("totalHits").and_then(|v| v.as_u64()).map(|n| n as usize);
            let results = data
                .get("results")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let papers = results.iter().take(limit).map(parse_work).collect();
            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }
}
