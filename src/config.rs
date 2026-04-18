use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub download_dir: PathBuf,
    pub crossref_email: Option<String>,
    pub semantic_scholar_api_key: Option<String>,
    pub unpaywall_email: String,
    pub core_api_key: Option<String>,
    pub pubmed_api_key: Option<String>,
    pub provider_timeout: Duration,
    pub max_parallel_providers: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Config {
    pub fn from_env() -> Self {
        let home = env::var("HOME")
            .or_else(|_| env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());

        let download_dir = env::var("RESEARCH_MCP_DOWNLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join("Downloads").join("papers"));

        let provider_timeout = env::var("RESEARCH_MCP_PROVIDER_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(Duration::from_secs_f64)
            .unwrap_or(Duration::from_secs(30));

        let max_parallel_providers = env::var("RESEARCH_MCP_MAX_PARALLEL_PROVIDERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);

        Self {
            download_dir,
            crossref_email: env::var("RESEARCH_MCP_CROSSREF_EMAIL").ok(),
            semantic_scholar_api_key: env::var("RESEARCH_MCP_SEMANTIC_SCHOLAR_API_KEY").ok(),
            unpaywall_email: env::var("RESEARCH_MCP_UNPAYWALL_EMAIL")
                .unwrap_or_else(|_| "user@example.com".into()),
            core_api_key: env::var("RESEARCH_MCP_CORE_API_KEY").ok(),
            pubmed_api_key: env::var("RESEARCH_MCP_PUBMED_API_KEY").ok(),
            provider_timeout,
            max_parallel_providers,
        }
    }
}
