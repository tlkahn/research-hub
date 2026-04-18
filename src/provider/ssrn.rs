use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use scraper::{Html, Selector};

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

pub struct SsrnProvider {
    base: ProviderBase,
}

impl SsrnProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_secs(1)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://papers.ssrn.com")
    }
}

#[async_trait]
impl Provider for SsrnProvider {
    fn name(&self) -> &str {
        "ssrn"
    }
    fn priority(&self) -> i32 {
        85
    }
    fn base_delay(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Keywords, SearchType::Title]
    }

    async fn search(
        &self,
        query: &str,
        _search_type: SearchType,
        limit: usize,
        _offset: usize,
    ) -> Result<ProviderResult> {
        let base = &self.base;
        retry("ssrn", 3, || async {
            base.rate_limiter.wait().await;

            let url = format!("{}/sol3/results.cfm", self.base_url());
            let resp = base
                .client
                .get(&url)
                .query(&[("txtKey_Words", query), ("npage", "1")])
                .header("User-Agent", "Mozilla/5.0 (compatible; research-bot)")
                .send()
                .await?;
            resp.error_for_status_ref()?;

            let html = resp.text().await?;
            let document = Html::parse_document(&html);
            let item_sel =
                Selector::parse(".result-item, .paper-result").unwrap();
            let title_sel =
                Selector::parse("a.title, .result-title a").unwrap();
            let author_sel =
                Selector::parse(".authors-list a, .author").unwrap();
            let abstract_sel =
                Selector::parse(".abstract-text, .description").unwrap();
            let id_re = Regex::new(r"abstract_id=(\d+)").unwrap();

            let mut papers = Vec::new();
            for item in document.select(&item_sel).take(limit) {
                let title_el = match item.select(&title_sel).next() {
                    Some(el) => el,
                    None => continue,
                };

                let title: String = title_el.text().collect::<String>().trim().to_string();
                let mut href = title_el
                    .value()
                    .attr("href")
                    .unwrap_or("")
                    .to_string();
                if !href.is_empty() && !href.starts_with("http") {
                    href = format!("{}{}", self.base_url(), href);
                }

                let authors: Vec<String> = item
                    .select(&author_sel)
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                let abstract_text = item
                    .select(&abstract_sel)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string());

                let ssrn_id = id_re.captures(&href).map(|c| c[1].to_string());
                let pdf_url = ssrn_id.as_ref().map(|id| {
                    format!(
                        "{}/sol3/Delivery.cfm?abstractid={id}",
                        self.base_url()
                    )
                });

                papers.push(Paper {
                    title,
                    authors,
                    abstract_text,
                    source: "ssrn".into(),
                    url: Some(href),
                    pdf_url,
                    ..Default::default()
                });
            }
            Ok(ProviderResult { papers, total_hits: None })
        })
        .await
    }
}
