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

#[derive(Debug, Clone)]
pub struct ProviderResult {
    pub papers: Vec<Paper>,
    pub total_hits: Option<usize>,
}

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
        offset: usize,
    ) -> Result<ProviderResult>;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_type_display() {
        assert_eq!(SearchType::Doi.to_string(), "DOI");
        assert_eq!(SearchType::Keywords.to_string(), "KEYWORDS");
        assert_eq!(SearchType::Author.to_string(), "AUTHOR");
        assert_eq!(SearchType::Title.to_string(), "TITLE");
    }

    #[test]
    fn test_search_type_equality() {
        assert_eq!(SearchType::Doi, SearchType::Doi);
        assert_ne!(SearchType::Doi, SearchType::Keywords);
    }

    #[test]
    fn test_search_type_serde_roundtrip() {
        let st = SearchType::Keywords;
        let json = serde_json::to_string(&st).unwrap();
        let deser: SearchType = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, st);
    }

    #[test]
    fn test_create_all_providers_count() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let providers = create_all_providers(client, config);
        assert_eq!(providers.len(), 13);
    }

    #[test]
    fn test_create_all_providers_sorted_descending() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let providers = create_all_providers(client, config);
        for i in 1..providers.len() {
            assert!(
                providers[i - 1].priority() >= providers[i].priority(),
                "{} (pri {}) should come before {} (pri {})",
                providers[i - 1].name(),
                providers[i - 1].priority(),
                providers[i].name(),
                providers[i].priority(),
            );
        }
    }

    #[test]
    fn test_create_all_providers_highest_is_openalex() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let providers = create_all_providers(client, config);
        assert_eq!(providers[0].name(), "openalex");
    }

    #[test]
    fn test_create_all_providers_lowest_is_scihub() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let providers = create_all_providers(client, config);
        assert_eq!(providers.last().unwrap().name(), "sci_hub");
    }

    #[tokio::test]
    async fn test_rate_limiter_allows_first_request() {
        let limiter = RateLimiter::new(Duration::from_millis(100));
        let start = Instant::now();
        limiter.wait().await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_rate_limiter_enforces_delay() {
        let limiter = RateLimiter::new(Duration::from_millis(100));
        limiter.wait().await;
        let start = Instant::now();
        limiter.wait().await;
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn test_retry_succeeds_immediately() {
        let result = retry("test", 3, || async { Ok::<_, crate::error::Error>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retry_fails_after_max_attempts() {
        let result = retry("test", 2, || async {
            Err::<i32, _>(crate::error::Error::provider("test", "always fails"))
        })
        .await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("always fails"));
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_failures() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();
        let result = retry("test", 3, move || {
            let attempt = attempt_clone.clone();
            async move {
                let n = attempt.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(crate::error::Error::provider("test", "not yet"))
                } else {
                    Ok(99)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 99);
    }

    #[test]
    fn test_provider_base_new() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let base = ProviderBase::new(client, config, Duration::from_millis(500));
        assert!(base.base_url.is_none());
    }

    #[test]
    fn test_provider_base_with_base_url() {
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let base = ProviderBase::new(client, config, Duration::from_millis(500))
            .with_base_url("http://localhost:8080".into());
        assert_eq!(base.base_url, Some("http://localhost:8080".into()));
    }
}
