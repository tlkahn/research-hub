use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::seq::SliceRandom;
use regex::Regex;
use scraper::{Html, Selector};

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType};

const MIRRORS: &[&str] = &[
    "https://sci-hub.se",
    "https://sci-hub.st",
    "https://sci-hub.ru",
    "https://sci-hub.ren",
    "https://sci-hub.mksa.top",
    "https://sci-hub.ee",
    "https://sci-hub.wf",
];

const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/120.0.0.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/121.0.0.0",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/119.0.0.0",
];

pub struct SciHubProvider {
    base: ProviderBase,
}

impl SciHubProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_secs(2)),
        }
    }

    fn get_mirror(&self) -> &str {
        if let Some(base) = &self.base.base_url {
            return base.as_str();
        }
        let mut rng = rand::thread_rng();
        MIRRORS.choose(&mut rng).unwrap_or(&MIRRORS[0])
    }

    fn normalize_url(url: &str, mirror: &str) -> String {
        if url.starts_with("//") {
            format!("https:{url}")
        } else if url.starts_with('/') {
            format!("{mirror}{url}")
        } else {
            url.to_string()
        }
    }

    async fn find_pdf(&self, doi: &str) -> Option<String> {
        let pdf_re = Regex::new(r#"(https?://[^\s"'<>]+\.pdf)"#).unwrap();

        for _ in 0..3 {
            let mirror = self.get_mirror().to_string();
            let ua = {
                let mut rng = rand::thread_rng();
                USER_AGENTS.choose(&mut rng).unwrap_or(&USER_AGENTS[0])
            };

            let url = format!("{mirror}/{doi}");
            let resp = self
                .base
                .client
                .get(&url)
                .header("User-Agent", *ua)
                .timeout(Duration::from_secs(15))
                .send()
                .await;

            let resp = match resp {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };

            let text = match resp.text().await {
                Ok(t) => t,
                Err(_) => continue,
            };

            let document = Html::parse_document(&text);

            // Try embed src
            if let Ok(sel) = Selector::parse("embed[src]")
                && let Some(el) = document.select(&sel).next()
                    && let Some(src) = el.value().attr("src") {
                        return Some(Self::normalize_url(src, &mirror));
                    }

            // Try iframe src
            if let Ok(sel) = Selector::parse("iframe[src]")
                && let Some(el) = document.select(&sel).next()
                    && let Some(src) = el.value().attr("src") {
                        return Some(Self::normalize_url(src, &mirror));
                    }

            // Try direct PDF link
            if let Ok(sel) = Selector::parse("a[href]") {
                for el in document.select(&sel) {
                    if let Some(href) = el.value().attr("href")
                        && href.contains(".pdf") {
                            return Some(Self::normalize_url(href, &mirror));
                        }
                }
            }

            // Try regex in page source
            if let Some(caps) = pdf_re.captures(&text) {
                return Some(caps[1].to_string());
            }
        }
        None
    }
}

#[async_trait]
impl Provider for SciHubProvider {
    fn name(&self) -> &str {
        "sci_hub"
    }
    fn priority(&self) -> i32 {
        10
    }
    fn base_delay(&self) -> Duration {
        Duration::from_secs(2)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Doi]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        _limit: usize,
    ) -> Result<ProviderResult> {
        if search_type != SearchType::Doi {
            return Ok(ProviderResult { papers: vec![], total_hits: None });
        }

        self.base.rate_limiter.wait().await;
        let doi = query.trim_start_matches("https://doi.org/");
        let pdf_url = self.find_pdf(doi).await;
        match pdf_url {
            Some(url) => Ok(ProviderResult {
                papers: vec![Paper {
                    title: format!("Paper (DOI: {doi})"),
                    doi: Some(doi.to_string()),
                    source: "sci_hub".into(),
                    url: Some(format!("https://doi.org/{doi}")),
                    pdf_url: Some(url),
                    ..Default::default()
                }],
                total_hits: None,
            }),
            None => Ok(ProviderResult { papers: vec![], total_hits: None }),
        }
    }

    async fn get_pdf_url(&self, doi: &str) -> Result<Option<String>> {
        Ok(self.find_pdf(doi).await)
    }
}
