use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

fn reconstruct_abstract(inv_index: Option<&serde_json::Value>) -> Option<String> {
    let map = inv_index?.as_object()?;
    let mut words: Vec<(i64, &str)> = Vec::new();
    for (word, positions) in map {
        if let Some(arr) = positions.as_array() {
            for pos in arr {
                if let Some(p) = pos.as_i64() {
                    words.push((p, word));
                }
            }
        }
    }
    if words.is_empty() {
        return None;
    }
    words.sort_by_key(|(pos, _)| *pos);
    Some(words.into_iter().map(|(_, w)| w).collect::<Vec<_>>().join(" "))
}

fn parse_work(work: &serde_json::Value) -> Paper {
    let mut authors = Vec::new();
    if let Some(authorships) = work.get("authorships").and_then(|v| v.as_array()) {
        for authorship in authorships {
            if let Some(name) = authorship
                .get("author")
                .and_then(|a| a.get("display_name"))
                .and_then(|n| n.as_str())
            {
                authors.push(name.to_string());
            }
        }
    }

    let doi = work
        .get("doi")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches("https://doi.org/"))
        .filter(|s| !s.is_empty())
        .map(String::from);

    let pdf_url = work
        .get("open_access")
        .and_then(|oa| oa.get("oa_url"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let journal = work
        .get("primary_location")
        .and_then(|loc| loc.get("source"))
        .and_then(|src| src.get("display_name"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let published_date = work
        .get("publication_date")
        .and_then(|v| v.as_str())
        .map(String::from);

    Paper {
        title: work
            .get("display_name")
            .or_else(|| work.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        authors,
        abstract_text: reconstruct_abstract(work.get("abstract_inverted_index")),
        doi,
        year: work
            .get("publication_year")
            .and_then(|v| v.as_i64())
            .map(|y| y as i32),
        published_date,
        source: "openalex".into(),
        url: work.get("id").and_then(|v| v.as_str()).map(String::from),
        pdf_url,
        journal,
        citation_count: work.get("cited_by_count").and_then(|v| v.as_i64()),
        publisher: work
            .get("primary_location")
            .and_then(|loc| loc.get("source"))
            .and_then(|src| src.get("host_organization_name"))
            .and_then(|v| v.as_str())
            .map(String::from),
        issn: work
            .get("primary_location")
            .and_then(|loc| loc.get("source"))
            .and_then(|src| src.get("issn_l"))
            .and_then(|v| v.as_str())
            .map(String::from),
        work_type: work.get("type").and_then(|v| v.as_str()).map(String::from),
        ..Default::default()
    }
}

pub struct OpenAlexProvider {
    base: ProviderBase,
}

impl OpenAlexProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(100)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.openalex.org")
    }
}

#[async_trait]
impl Provider for OpenAlexProvider {
    fn name(&self) -> &str {
        "openalex"
    }
    fn priority(&self) -> i32 {
        180
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(100)
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
        retry("openalex", 3, || async {
            base.rate_limiter.wait().await;

            let page = (offset / limit.max(1)) + 1;
            let mut params: Vec<(&str, String)> = vec![
                ("per_page", limit.to_string()),
                ("page", page.to_string()),
            ];

            if let Some(email) = &base.config.crossref_email {
                params.push(("mailto", email.clone()));
            }

            match search_type {
                SearchType::Doi => {
                    let doi = query.trim_start_matches("https://doi.org/");
                    params.push(("filter", format!("doi:{doi}")));
                }
                SearchType::Author => {
                    params.push((
                        "filter",
                        format!("authorships.author.display_name.search:{query}"),
                    ));
                }
                SearchType::Title => {
                    params.push(("filter", format!("display_name.search:{query}")));
                }
                SearchType::Keywords | SearchType::Isbn => {
                    params.push(("search", query.to_string()));
                }
            }

            params.push((
                "select",
                "id,doi,display_name,title,authorships,publication_year,publication_date,\
                 abstract_inverted_index,open_access,primary_location,cited_by_count,type"
                    .to_string(),
            ));

            let url = format!("{}/works", self.base_url());
            let resp = base.client.get(&url).query(&params).send().await?;
            resp.error_for_status_ref()?;
            let data: serde_json::Value = resp.json().await?;
            let total_hits = data
                .get("meta")
                .and_then(|m| m.get("count"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
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
