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

fn parse_item(item: &serde_json::Value) -> Paper {
    let mut authors = Vec::new();
    if let Some(author_list) = item.get("author").and_then(|v| v.as_array()) {
        for a in author_list {
            let given = a.get("given").and_then(|v| v.as_str()).unwrap_or("");
            let family = a.get("family").and_then(|v| v.as_str()).unwrap_or("");
            let name = format!("{given} {family}").trim().to_string();
            if !name.is_empty() {
                authors.push(name);
            }
        }
    }

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
