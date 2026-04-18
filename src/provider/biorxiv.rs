use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

fn parse_item(item: &serde_json::Value) -> Paper {
    let authors_str = item
        .get("authors")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let authors: Vec<String> = if authors_str.is_empty() {
        vec![]
    } else {
        authors_str
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let doi = item
        .get("doi")
        .and_then(|v| v.as_str())
        .map(String::from);

    let date_str = item.get("date").and_then(|v| v.as_str());
    let year = date_str
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<i32>().ok());
    let published_date = date_str.map(String::from);

    let version_str = item
        .get("version")
        .map(|v| {
            v.as_str()
                .map(String::from)
                .unwrap_or_else(|| v.as_i64().map(|n| n.to_string()).unwrap_or_else(|| "1".into()))
        })
        .unwrap_or_else(|| "1".into());

    let pdf_url = doi.as_ref().map(|d| {
        format!(
            "https://www.biorxiv.org/content/{d}v{}.full.pdf",
            version_str
        )
    });

    let url = doi
        .as_ref()
        .map(|d| format!("https://www.biorxiv.org/content/{d}"));

    Paper {
        title: item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        authors,
        abstract_text: item
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(String::from),
        doi,
        year,
        published_date,
        source: "biorxiv".into(),
        url,
        pdf_url,
        journal: Some("bioRxiv".into()),
        ..Default::default()
    }
}

pub struct BiorxivProvider {
    base: ProviderBase,
}

impl BiorxivProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(500)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.biorxiv.org/details/biorxiv")
    }
}

#[async_trait]
impl Provider for BiorxivProvider {
    fn name(&self) -> &str {
        "biorxiv"
    }
    fn priority(&self) -> i32 {
        75
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Doi]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        _limit: usize,
        _offset: usize,
    ) -> Result<ProviderResult> {
        if search_type != SearchType::Doi {
            return Ok(ProviderResult { papers: vec![], total_hits: None });
        }

        let base = &self.base;
        retry("biorxiv", 3, || async {
            base.rate_limiter.wait().await;
            let doi = query.trim_start_matches("https://doi.org/");
            let url = format!("{}/{}", self.base_url(), doi);
            let resp = base.client.get(&url).send().await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(ProviderResult { papers: vec![], total_hits: None });
            }
            resp.error_for_status_ref()?;

            let data: serde_json::Value = resp.json().await?;
            let collection = data
                .get("collection")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if collection.is_empty() {
                return Ok(ProviderResult { papers: vec![], total_hits: None });
            }
            Ok(ProviderResult { papers: vec![parse_item(&collection[0])], total_hits: None })
        })
        .await
    }

    async fn get_pdf_url(&self, doi: &str) -> Result<Option<String>> {
        let url = format!("{}/{}", self.base_url(), doi);
        let resp = self.base.client.get(&url).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r.json().await?;
                let collection = data
                    .get("collection")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if let Some(first) = collection.first() {
                    let version = first
                        .get("version")
                        .map(|v| {
                            v.as_str().map(String::from).unwrap_or_else(|| {
                                v.as_i64()
                                    .map(|n| n.to_string())
                                    .unwrap_or_else(|| "1".into())
                            })
                        })
                        .unwrap_or_else(|| "1".into());
                    Ok(Some(format!(
                        "https://www.biorxiv.org/content/{doi}v{version}.full.pdf"
                    )))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}
