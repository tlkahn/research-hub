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
    pub google_books_api_key: Option<String>,
    pub base_api_key: Option<String>,
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
            google_books_api_key: env::var("RESEARCH_MCP_GOOGLE_BOOKS_API_KEY").ok(),
            base_api_key: env::var("RESEARCH_MCP_BASE_API_KEY").ok(),
            provider_timeout,
            max_parallel_providers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_env_produces_valid_config() {
        let config = Config::from_env();
        assert!(config.max_parallel_providers > 0);
        assert!(config.provider_timeout.as_secs() > 0);
        assert!(!config.unpaywall_email.is_empty());
        assert!(!config.download_dir.as_os_str().is_empty());
    }

    #[test]
    fn test_config_download_dir_has_papers_suffix() {
        if env::var("RESEARCH_MCP_DOWNLOAD_DIR").is_err() {
            let config = Config::from_env();
            let path_str = config.download_dir.to_string_lossy().to_string();
            assert!(path_str.ends_with("Downloads/papers"));
        }
    }

    #[test]
    fn test_config_default_trait_delegates_to_from_env() {
        let a = Config::from_env();
        let b = Config::default();
        assert_eq!(a.max_parallel_providers, b.max_parallel_providers);
        assert_eq!(a.provider_timeout, b.provider_timeout);
        assert_eq!(a.unpaywall_email, b.unpaywall_email);
        assert_eq!(a.download_dir, b.download_dir);
    }

    #[test]
    fn test_config_unpaywall_default_email() {
        if env::var("RESEARCH_MCP_UNPAYWALL_EMAIL").is_err() {
            let config = Config::from_env();
            assert_eq!(config.unpaywall_email, "user@example.com");
        }
    }

    #[test]
    fn test_config_optional_keys_absent() {
        if env::var("RESEARCH_MCP_CROSSREF_EMAIL").is_err() {
            assert!(Config::from_env().crossref_email.is_none());
        }
        if env::var("RESEARCH_MCP_SEMANTIC_SCHOLAR_API_KEY").is_err() {
            assert!(Config::from_env().semantic_scholar_api_key.is_none());
        }
        if env::var("RESEARCH_MCP_CORE_API_KEY").is_err() {
            assert!(Config::from_env().core_api_key.is_none());
        }
        if env::var("RESEARCH_MCP_PUBMED_API_KEY").is_err() {
            assert!(Config::from_env().pubmed_api_key.is_none());
        }
        if env::var("RESEARCH_MCP_GOOGLE_BOOKS_API_KEY").is_err() {
            assert!(Config::from_env().google_books_api_key.is_none());
        }
        if env::var("RESEARCH_MCP_BASE_API_KEY").is_err() {
            assert!(Config::from_env().base_api_key.is_none());
        }
    }
}
