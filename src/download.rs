use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::join_all;
use regex::Regex;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::models::DownloadResult;
use crate::provider::{Provider, SearchType};
use crate::search::meta_search;

fn sanitize_filename(doi: &str) -> String {
    let re = Regex::new(r"[^\w\-.]").unwrap();
    format!("{}.pdf", re.replace_all(doi, "_"))
}

async fn download_pdf(client: &reqwest::Client, url: &str, dest: &Path) -> bool {
    let resp = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; research-bot)")
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return false,
    };

    if !content_type.contains("pdf") && !bytes.starts_with(b"%PDF-") {
        tracing::debug!(url, "Not a PDF response");
        return false;
    }

    if let Some(parent) = dest.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::debug!(error = %e, "Failed to create directory");
            return false;
        }

    match tokio::fs::write(dest, &bytes).await {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to write file");
            false
        }
    }
}

pub async fn download_paper(
    doi: &str,
    providers: &[Arc<dyn Provider>],
    client: &reqwest::Client,
    config: &Config,
    directory: Option<&Path>,
) -> DownloadResult {
    let dest_dir = directory
        .map(PathBuf::from)
        .unwrap_or_else(|| config.download_dir.clone());
    let dest = dest_dir.join(sanitize_filename(doi));

    if dest.exists() {
        return DownloadResult {
            doi: doi.to_string(),
            success: true,
            file_path: Some(dest.to_string_lossy().to_string()),
            error: None,
            source: Some("cache".into()),
        };
    }

    // Step 1: Search for the paper to find a pdf_url
    let result = meta_search(doi, providers, config, Some(SearchType::Doi), 5).await;
    for paper in &result.papers {
        if let Some(pdf_url) = &paper.pdf_url
            && download_pdf(client, pdf_url, &dest).await {
                return DownloadResult {
                    doi: doi.to_string(),
                    success: true,
                    file_path: Some(dest.to_string_lossy().to_string()),
                    error: None,
                    source: Some(paper.source.clone()),
                };
            }
    }

    // Step 2: Cascade through providers by priority
    let mut sorted: Vec<_> = providers.to_vec();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.priority()));

    for provider in &sorted {
        let name = provider.name().to_string();
        let pdf_url = match tokio::time::timeout(
            config.provider_timeout,
            provider.get_pdf_url(doi),
        )
        .await
        {
            Ok(Ok(Some(url))) => url,
            _ => continue,
        };

        if download_pdf(client, &pdf_url, &dest).await {
            return DownloadResult {
                doi: doi.to_string(),
                success: true,
                file_path: Some(dest.to_string_lossy().to_string()),
                error: None,
                source: Some(name),
            };
        }
    }

    DownloadResult {
        doi: doi.to_string(),
        success: false,
        file_path: None,
        error: Some(
            "No PDF found across all providers. \
             Try searching for the paper title to find alternative access."
                .into(),
        ),
        source: None,
    }
}

pub async fn download_papers_batch(
    papers: &[serde_json::Value],
    providers: &[Arc<dyn Provider>],
    client: &reqwest::Client,
    config: &Config,
    max_concurrent: usize,
    directory: Option<&Path>,
) -> Vec<DownloadResult> {
    let max_concurrent = max_concurrent.clamp(1, 100);
    let sem = Arc::new(Semaphore::new(max_concurrent));

    let tasks: Vec<_> = papers
        .iter()
        .map(|paper_spec| {
            let sem = sem.clone();
            let providers = providers.to_vec();
            let client = client.clone();
            let config = config.clone();
            let paper_spec = paper_spec.clone();
            let directory = directory.map(PathBuf::from);
            async move {
                let _permit = sem.acquire().await.unwrap();
                let doi = paper_spec
                    .get("doi")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if doi.is_empty() {
                    return DownloadResult {
                        doi: String::new(),
                        success: false,
                        file_path: None,
                        error: Some("Missing DOI".into()),
                        source: None,
                    };
                }

                let paper_dir = directory.or_else(|| {
                    paper_spec
                        .get("directory")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                });

                download_paper(
                    doi,
                    &providers,
                    &client,
                    &config,
                    paper_dir.as_deref(),
                )
                .await
            }
        })
        .collect();

    join_all(tasks).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_basic_doi() {
        assert_eq!(sanitize_filename("10.1234/test"), "10.1234_test.pdf");
    }

    #[test]
    fn test_sanitize_filename_preserves_dots_hyphens() {
        let result = sanitize_filename("10.1038/s41586-021-03819-2");
        assert_eq!(result, "10.1038_s41586-021-03819-2.pdf");
    }

    #[test]
    fn test_sanitize_filename_complex_doi() {
        let result = sanitize_filename("10.1145/3295222.3295349");
        assert_eq!(result, "10.1145_3295222.3295349.pdf");
    }

    #[test]
    fn test_sanitize_filename_special_chars() {
        let result = sanitize_filename("10.1234/(test)&value=1");
        assert!(!result.contains('('));
        assert!(!result.contains(')'));
        assert!(!result.contains('&'));
        assert!(!result.contains('='));
        assert!(result.ends_with(".pdf"));
    }

    #[test]
    fn test_sanitize_filename_unicode() {
        let result = sanitize_filename("10.1234/tëst");
        assert!(result.ends_with(".pdf"));
        assert!(!result.contains('/'));
    }

    #[tokio::test]
    async fn test_download_paper_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join(sanitize_filename("10.1234/cached"));
        tokio::fs::write(&dest, b"%PDF-1.4 cached").await.unwrap();

        let config = Config {
            download_dir: tmp.path().to_path_buf(),
            ..Config::from_env()
        };
        let client = reqwest::Client::new();
        let providers: Vec<Arc<dyn Provider>> = vec![];

        let result = download_paper("10.1234/cached", &providers, &client, &config, None).await;
        assert!(result.success);
        assert_eq!(result.source, Some("cache".into()));
    }

    #[tokio::test]
    async fn test_download_paper_no_providers() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            download_dir: tmp.path().to_path_buf(),
            ..Config::from_env()
        };
        let client = reqwest::Client::new();
        let providers: Vec<Arc<dyn Provider>> = vec![];

        let result =
            download_paper("10.1234/nonexistent", &providers, &client, &config, None).await;
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_download_papers_batch_empty_doi() {
        let config = Config::from_env();
        let client = reqwest::Client::new();
        let providers: Vec<Arc<dyn Provider>> = vec![];
        let specs = vec![serde_json::json!({"title": "no doi"})];

        let results =
            download_papers_batch(&specs, &providers, &client, &config, 3, None).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert_eq!(results[0].error, Some("Missing DOI".into()));
    }

    #[tokio::test]
    async fn test_download_papers_batch_clamps_concurrency() {
        let config = Config::from_env();
        let client = reqwest::Client::new();
        let providers: Vec<Arc<dyn Provider>> = vec![];
        let specs: Vec<serde_json::Value> = vec![];

        let results = download_papers_batch(&specs, &providers, &client, &config, 0, None).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_download_paper_custom_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_dir = tmp.path().join("custom");

        let config = Config::from_env();
        let client = reqwest::Client::new();
        let providers: Vec<Arc<dyn Provider>> = vec![];

        let result = download_paper(
            "10.1234/custom",
            &providers,
            &client,
            &config,
            Some(custom_dir.as_path()),
        )
        .await;
        assert!(!result.success);
    }
}
