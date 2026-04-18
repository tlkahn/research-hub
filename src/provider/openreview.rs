use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

fn val_str(content: &serde_json::Value, field: &str) -> Option<String> {
    let v = content.get(field)?;
    if let Some(obj) = v.as_object() {
        obj.get("value")
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        v.as_str().map(String::from)
    }
}

fn parse_note(note: &serde_json::Value) -> Paper {
    let content = note.get("content").cloned().unwrap_or_default();

    let title = val_str(&content, "title").unwrap_or_else(|| "Unknown".into());
    let abstract_text = val_str(&content, "abstract");

    let authors_raw = content.get("authors");
    let mut authors = Vec::new();
    if let Some(raw) = authors_raw {
        if let Some(v) = raw.as_object().and_then(|o| o.get("value")) {
            if let Some(arr) = v.as_array() {
                authors = arr.iter().filter_map(|a| a.as_str().map(String::from)).collect();
            }
        } else if let Some(arr) = raw.as_array() {
            authors = arr.iter().filter_map(|a| a.as_str().map(String::from)).collect();
        } else if let Some(s) = raw.as_str() {
            authors.push(s.to_string());
        }
    }

    let venue = val_str(&content, "venue").or_else(|| val_str(&content, "venueid"));

    let year = note
        .get("cdate")
        .or_else(|| note.get("pdate"))
        .and_then(|v| v.as_i64())
        .map(|ms| {
            let secs = ms / 1000;
            
            chrono_lite_year(secs)
        });

    let note_id = note.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let url = if !note_id.is_empty() {
        Some(format!("https://openreview.net/forum?id={note_id}"))
    } else {
        None
    };
    let pdf_url = if !note_id.is_empty() {
        Some(format!("https://openreview.net/pdf?id={note_id}"))
    } else {
        None
    };

    Paper {
        title,
        authors,
        abstract_text,
        year,
        source: "openreview".into(),
        url,
        pdf_url,
        journal: venue,
        ..Default::default()
    }
}

fn chrono_lite_year(unix_secs: i64) -> i32 {
    // Simple year extraction from unix timestamp
    let days = unix_secs / 86400 + 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    y as i32
}

pub struct OpenReviewProvider {
    base: ProviderBase,
}

impl OpenReviewProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_millis(200)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://api.openreview.net")
    }
}

#[async_trait]
impl Provider for OpenReviewProvider {
    fn name(&self) -> &str {
        "openreview"
    }
    fn priority(&self) -> i32 {
        85
    }
    fn base_delay(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Keywords, SearchType::Title]
    }

    async fn search(
        &self,
        query: &str,
        _search_type: SearchType,
        limit: usize,
    ) -> Result<ProviderResult> {
        let base = &self.base;
        retry("openreview", 3, || async {
            base.rate_limiter.wait().await;

            let url = format!("{}/notes/search", self.base_url());
            let resp = base
                .client
                .get(&url)
                .query(&[("query", query), ("limit", &limit.to_string())])
                .send()
                .await?;
            resp.error_for_status_ref()?;

            let data: serde_json::Value = resp.json().await?;
            let total_hits = data.get("count").and_then(|v| v.as_u64()).map(|n| n as usize);
            let notes = data
                .get("notes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let papers = notes.iter().take(limit).map(parse_note).collect();
            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }
}
