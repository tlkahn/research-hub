use std::collections::HashSet;
use std::sync::Arc;

use futures::future::join_all;
use regex::Regex;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::models::{Paper, ProviderHits, SearchResult, SortOrder};
use crate::provider::{Provider, ProviderResult, SearchType};

pub fn detect_search_type(query: &str) -> SearchType {
    let q = query.trim();
    let doi_re = Regex::new(r"^(?:https?://doi\.org/)?10\.\d{4,}/\S+$").unwrap();
    if doi_re.is_match(q) {
        return SearchType::Doi;
    }
    // ISBN check: strip hyphens, then match ISBN-10 or ISBN-13
    let stripped = q.replace('-', "");
    let isbn_re = Regex::new(r"^(?:\d{9}[\dXx]|97[89]\d{10})$").unwrap();
    if isbn_re.is_match(&stripped) {
        return SearchType::Isbn;
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
    let mut seen_isbns: HashSet<String> = HashSet::new();
    let mut seen_oclcs: HashSet<String> = HashSet::new();
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

        if let Some(isbn) = &paper.isbn {
            let isbn_key = isbn.replace('-', "").to_lowercase();
            if seen_isbns.contains(&isbn_key) {
                continue;
            }
            seen_isbns.insert(isbn_key);
        }

        if let Some(oclc) = &paper.oclc {
            let oclc_key = oclc.trim().to_lowercase();
            if seen_oclcs.contains(&oclc_key) {
                continue;
            }
            seen_oclcs.insert(oclc_key);
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

fn normalize_dates(papers: &mut [Paper]) {
    for paper in papers.iter_mut() {
        if paper.published_date.is_none()
            && let Some(y) = paper.year
        {
            paper.published_date = Some(format!("{y:04}-01-01"));
        }
    }
}

fn sort_papers(papers: &mut [Paper], sort: SortOrder) {
    match sort {
        SortOrder::Relevance => {}
        SortOrder::Date => {
            papers.sort_by(|a, b| b.published_date.cmp(&a.published_date));
        }
        SortOrder::DateAsc => {
            papers.sort_by(|a, b| a.published_date.cmp(&b.published_date));
        }
        SortOrder::Citations => {
            papers.sort_by_key(|p| std::cmp::Reverse(p.citation_count));
        }
    }
}

pub async fn meta_search(
    query: &str,
    providers: &[Arc<dyn Provider>],
    config: &Config,
    search_type: Option<SearchType>,
    limit: usize,
    offset: usize,
    sort: SortOrder,
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
            offset,
            sort: sort.to_string(),
            total_hits: None,
            provider_hits: vec![],
            providers_searched: vec![],
            providers_failed: vec![],
        };
    }

    let single_provider = applicable.len() == 1;
    let (provider_offset, provider_limit) = if single_provider {
        (offset, limit)
    } else {
        (0, offset + limit)
    };

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
                    provider.search(&query, search_type, provider_limit, provider_offset),
                )
                .await
                {
                    Ok(Ok(result)) => (name, Ok(result)),
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
    let mut provider_hits = Vec::new();

    for (name, result) in results {
        match result {
            Ok(ProviderResult { papers, total_hits }) => {
                all_papers.extend(papers);
                if let Some(hits) = total_hits {
                    provider_hits.push(ProviderHits {
                        provider: name.clone(),
                        total_hits: hits,
                    });
                }
                providers_searched.push(name);
            }
            Err(()) => {
                providers_failed.push(name);
            }
        }
    }

    providers_searched.sort();
    providers_failed.sort();

    let total_hits = if provider_hits.is_empty() {
        None
    } else {
        Some(provider_hits.iter().map(|ph| ph.total_hits).sum())
    };

    let mut deduped = deduplicate(all_papers);
    normalize_dates(&mut deduped);
    sort_papers(&mut deduped, sort);

    if !single_provider && offset > 0 {
        if offset >= deduped.len() {
            deduped.clear();
        } else {
            deduped = deduped.split_off(offset);
        }
    }
    deduped.truncate(limit);

    SearchResult {
        query: query.to_string(),
        search_type: search_type.to_string(),
        total_results: deduped.len(),
        papers: deduped,
        offset,
        sort: sort.to_string(),
        total_hits,
        provider_hits,
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

    #[test]
    fn test_detect_search_type_doi_url_variant() {
        assert_eq!(
            detect_search_type("10.1038/s41586-021-03819-2"),
            SearchType::Doi
        );
    }

    #[test]
    fn test_detect_search_type_not_doi() {
        assert_eq!(detect_search_type("10.not-a-doi"), SearchType::Keywords);
    }

    #[test]
    fn test_detect_search_type_whitespace() {
        assert_eq!(detect_search_type("  Smith, J  "), SearchType::Author);
        assert_eq!(
            detect_search_type("  10.1234/test  "),
            SearchType::Doi
        );
    }

    #[test]
    fn test_normalize_title_empty() {
        assert_eq!(normalize_title(""), "");
    }

    #[test]
    fn test_normalize_title_mixed_case_and_whitespace() {
        assert_eq!(
            normalize_title("  The  QUICK\tbrown FOX  "),
            "the quick brown fox"
        );
    }

    #[test]
    fn test_deduplicate_empty() {
        let result = deduplicate(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_deduplicate_preserves_order() {
        let papers = vec![
            Paper {
                title: "First".into(),
                doi: Some("10.1/a".into()),
                ..Default::default()
            },
            Paper {
                title: "Second".into(),
                doi: Some("10.1/b".into()),
                ..Default::default()
            },
            Paper {
                title: "Third".into(),
                doi: Some("10.1/c".into()),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].title, "First");
        assert_eq!(result[2].title, "Third");
    }

    #[test]
    fn test_deduplicate_doi_case_insensitive() {
        let papers = vec![
            Paper {
                title: "Paper A".into(),
                doi: Some("10.1234/TEST".into()),
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
    }

    #[test]
    fn test_deduplicate_no_doi_deduplicates_by_title() {
        let papers = vec![
            Paper {
                title: "Same Paper".into(),
                source: "source1".into(),
                ..Default::default()
            },
            Paper {
                title: "same paper".into(),
                source: "source2".into(),
                ..Default::default()
            },
            Paper {
                title: "Different Paper".into(),
                source: "source3".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].source, "source1");
    }

    #[test]
    fn test_detect_search_type_isbn13() {
        assert_eq!(detect_search_type("9780306406157"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_isbn13_hyphenated() {
        assert_eq!(detect_search_type("978-0-306-40615-7"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_isbn10() {
        assert_eq!(detect_search_type("0306406152"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_isbn10_with_x() {
        assert_eq!(detect_search_type("123456789X"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_isbn10_hyphenated() {
        assert_eq!(detect_search_type("0-306-40615-2"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_not_isbn() {
        assert_eq!(detect_search_type("12345"), SearchType::Keywords);
        assert_eq!(detect_search_type("12345678901234"), SearchType::Keywords);
    }

    #[test]
    fn test_deduplicate_by_isbn() {
        let papers = vec![
            Paper { title: "Book A".into(), isbn: Some("978-0-306-40615-7".into()), ..Default::default() },
            Paper { title: "Book B".into(), isbn: Some("978-0-306-40615-7".into()), ..Default::default() },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Book A");
    }

    #[test]
    fn test_deduplicate_by_oclc() {
        let papers = vec![
            Paper { title: "Item A".into(), oclc: Some("12345678".into()), ..Default::default() },
            Paper { title: "Item B".into(), oclc: Some("12345678".into()), ..Default::default() },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Item A");
    }

    #[test]
    fn test_deduplicate_chain_doi_isbn_oclc_title() {
        let papers = vec![
            Paper { title: "A".into(), doi: Some("10.1/a".into()), ..Default::default() },
            Paper { title: "A dup".into(), doi: Some("10.1/a".into()), ..Default::default() },
            Paper { title: "B".into(), isbn: Some("978-0-306-40615-7".into()), ..Default::default() },
            Paper { title: "B dup".into(), isbn: Some("978-0-306-40615-7".into()), ..Default::default() },
            Paper { title: "C".into(), oclc: Some("12345678".into()), ..Default::default() },
            Paper { title: "C dup".into(), oclc: Some("12345678".into()), ..Default::default() },
            Paper { title: "D".into(), ..Default::default() },
            Paper { title: "D".into(), ..Default::default() },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 4);
    }
}
