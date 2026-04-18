use std::collections::HashSet;
use std::sync::Arc;

use futures::future::join_all;
use regex::Regex;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::models::{Paper, SearchResult};
use crate::provider::{Provider, SearchType};

pub fn detect_search_type(query: &str) -> SearchType {
    let q = query.trim();
    let doi_re = Regex::new(r"^(?:https?://doi\.org/)?10\.\d{4,}/\S+$").unwrap();
    if doi_re.is_match(q) {
        return SearchType::Doi;
    }
    let author_re = Regex::new(r"^[A-Z][a-z]+,\s*[A-Z]").unwrap();
    if author_re.is_match(q) {
        return SearchType::Author;
    }
    SearchType::Keywords
}

fn normalize_title(title: &str) -> String {
    let re = Regex::new(r"\s+").unwrap();
    re.replace_all(title.to_lowercase().trim(), " ").to_string()
}

fn deduplicate(papers: Vec<Paper>) -> Vec<Paper> {
    let mut seen_dois: HashSet<String> = HashSet::new();
    let mut seen_titles: HashSet<String> = HashSet::new();
    let mut result = Vec::new();

    for paper in papers {
        if let Some(doi) = &paper.doi {
            let doi_key = doi.to_lowercase();
            if seen_dois.contains(&doi_key) {
                continue;
            }
            seen_dois.insert(doi_key);
        }

        let norm_title = normalize_title(&paper.title);
        if seen_titles.contains(&norm_title) {
            continue;
        }
        seen_titles.insert(norm_title);

        result.push(paper);
    }

    result
}

pub async fn meta_search(
    query: &str,
    providers: &[Arc<dyn Provider>],
    config: &Config,
    search_type: Option<SearchType>,
    limit: usize,
) -> SearchResult {
    let search_type = search_type.unwrap_or_else(|| detect_search_type(query));

    let applicable: Vec<_> = providers
        .iter()
        .filter(|p| p.supported_search_types().contains(&search_type))
        .collect();

    if applicable.is_empty() {
        return SearchResult {
            query: query.to_string(),
            search_type: search_type.to_string(),
            papers: vec![],
            total_results: 0,
            providers_searched: vec![],
            providers_failed: vec![],
        };
    }

    let sem = Arc::new(Semaphore::new(config.max_parallel_providers));
    let timeout = config.provider_timeout;

    let tasks: Vec<_> = applicable
        .into_iter()
        .map(|provider| {
            let sem = sem.clone();
            let provider = provider.clone();
            let query = query.to_string();
            async move {
                let _permit = sem.acquire().await.unwrap();
                let name = provider.name().to_string();
                match tokio::time::timeout(
                    timeout,
                    provider.search(&query, search_type, limit),
                )
                .await
                {
                    Ok(Ok(papers)) => (name, Ok(papers)),
                    Ok(Err(e)) => {
                        tracing::warn!(provider = %name, error = %e, "provider failed");
                        (name, Err(()))
                    }
                    Err(_) => {
                        tracing::warn!(provider = %name, "provider timed out");
                        (name, Err(()))
                    }
                }
            }
        })
        .collect();

    let results = join_all(tasks).await;

    let mut all_papers = Vec::new();
    let mut providers_searched = Vec::new();
    let mut providers_failed = Vec::new();

    for (name, result) in results {
        match result {
            Ok(papers) => {
                all_papers.extend(papers);
                providers_searched.push(name);
            }
            Err(()) => {
                providers_failed.push(name);
            }
        }
    }

    providers_searched.sort();
    providers_failed.sort();

    let mut deduped = deduplicate(all_papers);
    deduped.truncate(limit);

    SearchResult {
        query: query.to_string(),
        search_type: search_type.to_string(),
        papers: deduped.clone(),
        total_results: deduped.len(),
        providers_searched,
        providers_failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_search_type_doi() {
        assert_eq!(detect_search_type("10.1234/test"), SearchType::Doi);
        assert_eq!(
            detect_search_type("https://doi.org/10.1234/test"),
            SearchType::Doi
        );
    }

    #[test]
    fn test_detect_search_type_author() {
        assert_eq!(detect_search_type("Smith, J"), SearchType::Author);
        assert_eq!(
            detect_search_type("Johnson, Alice"),
            SearchType::Author
        );
    }

    #[test]
    fn test_detect_search_type_keywords() {
        assert_eq!(
            detect_search_type("transformer attention"),
            SearchType::Keywords
        );
        assert_eq!(
            detect_search_type("machine learning"),
            SearchType::Keywords
        );
    }

    #[test]
    fn test_normalize_title() {
        assert_eq!(
            normalize_title("  Hello   World  "),
            "hello world"
        );
    }

    #[test]
    fn test_deduplicate_by_doi() {
        let papers = vec![
            Paper {
                title: "Paper A".into(),
                doi: Some("10.1234/test".into()),
                ..Default::default()
            },
            Paper {
                title: "Paper B".into(),
                doi: Some("10.1234/test".into()),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Paper A");
    }

    #[test]
    fn test_deduplicate_by_title() {
        let papers = vec![
            Paper {
                title: "Same Title".into(),
                ..Default::default()
            },
            Paper {
                title: "  same  title  ".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
    }
}
