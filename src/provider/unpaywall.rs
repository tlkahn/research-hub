use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

pub struct UnpaywallProvider {
    base: ProviderBase,
}

impl UnpaywallProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(100)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.unpaywall.org/v2")
    }
}

#[async_trait]
impl Provider for UnpaywallProvider {
    fn name(&self) -> &str {
        "unpaywall"
    }
    fn priority(&self) -> i32 {
        87
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(100)
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
        retry("unpaywall", 3, || async {
            base.rate_limiter.wait().await;
            let doi = query.trim_start_matches("https://doi.org/");
            let url = format!(
                "{}/{}?email={}",
                self.base_url(),
                doi,
                &base.config.unpaywall_email
            );
            let resp = base.client.get(&url).send().await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(ProviderResult { papers: vec![], total_hits: None });
            }
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;

            let pdf_url = data
                .get("best_oa_location")
                .and_then(|loc| {
                    loc.get("url_for_pdf")
                        .or_else(|| loc.get("url"))
                })
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut authors = Vec::new();
            if let Some(arr) = data.get("z_authors").and_then(|v| v.as_array()) {
                for a in arr {
                    let given = a.get("given").and_then(|v| v.as_str()).unwrap_or("");
                    let family = a.get("family").and_then(|v| v.as_str()).unwrap_or("");
                    let name = format!("{given} {family}").trim().to_string();
                    if !name.is_empty() {
                        authors.push(name);
                    }
                }
            }

            Ok(ProviderResult {
                papers: vec![Paper {
                    title: data
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                    authors,
                    doi: Some(doi.to_string()),
                    year: data.get("year").and_then(|v| v.as_i64()).map(|y| y as i32),
                    published_date: data
                        .get("published_date")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    source: "unpaywall".into(),
                    url: Some(format!("https://doi.org/{doi}")),
                    pdf_url,
                    journal: data
                        .get("journal_name")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    ..Default::default()
                }],
                total_hits: None,
            })
        })
        .await
    }

    async fn get_pdf_url(&self, doi: &str) -> Result<Option<String>> {
        let url = format!(
            "{}/{}?email={}",
            self.base_url(),
            doi,
            &self.base.config.unpaywall_email
        );
        let resp = self.base.client.get(&url).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r.json().await?;
                Ok(data
                    .get("best_oa_location")
                    .and_then(|loc| loc.get("url_for_pdf").or_else(|| loc.get("url")))
                    .and_then(|v| v.as_str())
                    .map(String::from))
            }
            _ => Ok(None),
        }
    }
}
