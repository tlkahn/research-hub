use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use scraper::{Html, Selector};

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

pub struct ResearchGateProvider {
    base: ProviderBase,
}

impl ResearchGateProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_secs(3)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://www.researchgate.net")
    }
}

#[async_trait]
impl Provider for ResearchGateProvider {
    fn name(&self) -> &str {
        "researchgate"
    }
    fn priority(&self) -> i32 {
        70
    }
    fn base_delay(&self) -> Duration {
        Duration::from_secs(3)
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
        retry("researchgate", 2, || async {
            base.rate_limiter.wait().await;

            let url = format!("{}/search/publication", self.base_url());
            let resp = base
                .client
                .get(&url)
                .query(&[("q", query)])
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
                )
                .header("Accept", "text/html,application/xhtml+xml")
                .header("Accept-Language", "en-US,en;q=0.9")
                .send()
                .await?;
            resp.error_for_status_ref()?;

            let html = resp.text().await?;
            let document = Html::parse_document(&html);
            let item_sel = Selector::parse(
                "[data-testid=\"search-result\"], .search-result",
            )
            .unwrap();
            let title_sel = Selector::parse(
                "[data-testid=\"publication-title\"] a, \
                 .publication-title a, \
                 a[href*=\"/publication/\"]",
            )
            .unwrap();
            let author_sel = Selector::parse(
                "[data-testid=\"author-name\"], \
                 .author-name, \
                 a[href*=\"/profile/\"]",
            )
            .unwrap();
            let abstract_sel =
                Selector::parse("[data-testid=\"abstract\"], .abstract").unwrap();

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

                papers.push(Paper {
                    title,
                    authors,
                    abstract_text,
                    source: "researchgate".into(),
                    url: if href.is_empty() { None } else { Some(href) },
                    ..Default::default()
                });
            }
            Ok(ProviderResult { papers, total_hits: None })
        })
        .await
    }
}
