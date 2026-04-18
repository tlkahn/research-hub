pub mod arxiv;
pub mod biorxiv;
pub mod core_api;
pub mod crossref;
pub mod mdpi;
pub mod openalex;
pub mod openreview;
pub mod pubmed;
pub mod researchgate;
pub mod sci_hub;
pub mod semantic_scholar;
pub mod ssrn;
pub mod unpaywall;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SearchType {
    Doi,
    Keywords,
    Author,
    Title,
}

impl std::fmt::Display for SearchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Doi => write!(f, "DOI"),
            Self::Keywords => write!(f, "KEYWORDS"),
            Self::Author => write!(f, "AUTHOR"),
            Self::Title => write!(f, "TITLE"),
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;
    fn base_delay(&self) -> Duration;
    fn supported_search_types(&self) -> &[SearchType];
    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
    ) -> Result<Vec<Paper>>;
    async fn get_pdf_url(&self, _doi: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

pub struct RateLimiter {
    last_request: Mutex<Instant>,
    delay: Duration,
}

impl RateLimiter {
    pub fn new(delay: Duration) -> Self {
        Self {
            last_request: Mutex::new(Instant::now() - delay),
            delay,
        }
    }

    pub async fn wait(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.delay {
            tokio::time::sleep(self.delay - elapsed).await;
        }
        *last = Instant::now();
    }
}

pub struct ProviderBase {
    pub client: reqwest::Client,
    pub config: Arc<Config>,
    pub rate_limiter: RateLimiter,
    pub base_url: Option<String>,
}

impl ProviderBase {
    pub fn new(client: reqwest::Client, config: Arc<Config>, delay: Duration) -> Self {
        Self {
            client,
            config,
            rate_limiter: RateLimiter::new(delay),
            base_url: None,
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = Some(url);
        self
    }
}

pub async fn retry<F, Fut, T>(name: &str, max_attempts: u32, f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                tracing::debug!(provider = name, attempt, error = %e, "retry");
                last_err = Some(e);
                if attempt + 1 < max_attempts {
                    let backoff = Duration::from_millis(1000 * 2u64.pow(attempt));
                    let max = Duration::from_secs(10);
                    tokio::time::sleep(backoff.min(max)).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}

pub fn create_all_providers(
    client: reqwest::Client,
    config: Arc<Config>,
) -> Vec<Arc<dyn Provider>> {
    let mut providers: Vec<Arc<dyn Provider>> = vec![
        Arc::new(openalex::OpenAlexProvider::new(client.clone(), config.clone())),
        Arc::new(crossref::CrossrefProvider::new(client.clone(), config.clone())),
        Arc::new(pubmed::PubMedProvider::new(client.clone(), config.clone())),
        Arc::new(semantic_scholar::SemanticScholarProvider::new(client.clone(), config.clone())),
        Arc::new(unpaywall::UnpaywallProvider::new(client.clone(), config.clone())),
        Arc::new(core_api::CoreProvider::new(client.clone(), config.clone())),
        Arc::new(openreview::OpenReviewProvider::new(client.clone(), config.clone())),
        Arc::new(ssrn::SsrnProvider::new(client.clone(), config.clone())),
        Arc::new(arxiv::ArxivProvider::new(client.clone(), config.clone())),
        Arc::new(biorxiv::BiorxivProvider::new(client.clone(), config.clone())),
        Arc::new(mdpi::MdpiProvider::new(client.clone(), config.clone())),
        Arc::new(researchgate::ResearchGateProvider::new(client.clone(), config.clone())),
        Arc::new(sci_hub::SciHubProvider::new(client.clone(), config.clone())),
    ];
    providers.sort_by_key(|p| std::cmp::Reverse(p.priority()));
    providers
}
