use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, SearchType, retry};

pub struct PubMedProvider {
    base: ProviderBase,
}

impl PubMedProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(350)),
        }
    }

    fn esearch_url(&self) -> String {
        let base = self
            .base
            .base_url
            .as_deref()
            .unwrap_or("https://eutils.ncbi.nlm.nih.gov/entrez/eutils");
        format!("{base}/esearch.fcgi")
    }

    fn esummary_url(&self) -> String {
        let base = self
            .base
            .base_url
            .as_deref()
            .unwrap_or("https://eutils.ncbi.nlm.nih.gov/entrez/eutils");
        format!("{base}/esummary.fcgi")
    }

    fn api_params(&self) -> Vec<(&str, String)> {
        let mut params = Vec::new();
        if let Some(key) = &self.base.config.pubmed_api_key {
            params.push(("api_key", key.clone()));
        }
        params
    }
}

#[async_trait]
impl Provider for PubMedProvider {
    fn name(&self) -> &str {
        "pubmed"
    }
    fn priority(&self) -> i32 {
        89
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(350)
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
    ) -> Result<Vec<Paper>> {
        let base = &self.base;
        retry("pubmed", 3, || async {
            base.rate_limiter.wait().await;

            let term = match search_type {
                SearchType::Doi => format!("{query}[DOI]"),
                SearchType::Author => format!("{query}[Author]"),
                SearchType::Title => format!("{query}[Title]"),
                SearchType::Keywords => query.to_string(),
            };

            let mut params = self.api_params();
            params.push(("db", "pubmed".into()));
            params.push(("term", term));
            params.push(("retmax", limit.to_string()));
            params.push(("retmode", "json".into()));

            let resp = base
                .client
                .get(self.esearch_url())
                .query(&params)
                .send()
                .await?;
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;

            let id_list: Vec<String> = data
                .get("esearchresult")
                .and_then(|r| r.get("idlist"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            if id_list.is_empty() {
                return Ok(vec![]);
            }

            base.rate_limiter.wait().await;

            let mut sum_params = self.api_params();
            sum_params.push(("db", "pubmed".into()));
            sum_params.push(("id", id_list.join(",")));
            sum_params.push(("retmode", "json".into()));

            let resp = base
                .client
                .get(self.esummary_url())
                .query(&sum_params)
                .send()
                .await?;
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;
            let result = data.get("result").cloned().unwrap_or_default();

            let mut papers = Vec::new();
            for pmid in &id_list {
                let doc = match result.get(pmid).and_then(|v| v.as_object()) {
                    Some(d) => serde_json::Value::Object(d.clone()),
                    None => continue,
                };

                let mut authors = Vec::new();
                if let Some(arr) = doc.get("authors").and_then(|v| v.as_array()) {
                    for a in arr {
                        if let Some(name) = a.get("name").and_then(|v| v.as_str()) {
                            authors.push(name.to_string());
                        }
                    }
                }

                let mut doi = None;
                let mut pmc_id = None;
                if let Some(ids) = doc.get("articleids").and_then(|v| v.as_array()) {
                    for art_id in ids {
                        let id_type = art_id.get("idtype").and_then(|v| v.as_str());
                        let value = art_id
                            .get("value")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        match id_type {
                            Some("doi") if doi.is_none() => doi = value,
                            Some("pmc") if pmc_id.is_none() => pmc_id = value,
                            _ => {}
                        }
                    }
                }

                let pdf_url = pmc_id
                    .as_ref()
                    .map(|id| format!("https://www.ncbi.nlm.nih.gov/pmc/articles/{id}/pdf/"));

                let year = doc
                    .get("pubdate")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.get(..4))
                    .and_then(|s| s.parse::<i32>().ok());

                papers.push(Paper {
                    title: doc
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                    authors,
                    doi,
                    year,
                    source: "pubmed".into(),
                    url: Some(format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/")),
                    pdf_url,
                    journal: doc
                        .get("fulljournalname")
                        .or_else(|| doc.get("source"))
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    volume: doc.get("volume").and_then(|v| v.as_str()).map(String::from),
                    issue: doc.get("issue").and_then(|v| v.as_str()).map(String::from),
                    pages: doc.get("pages").and_then(|v| v.as_str()).map(String::from),
                    ..Default::default()
                });
            }
            Ok(papers)
        })
        .await
    }
}
