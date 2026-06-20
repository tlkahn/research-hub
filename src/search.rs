use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use futures::future::join_all;
use regex::Regex;
use tokio::sync::Semaphore;
use unicode_normalization::UnicodeNormalization;

use crate::config::Config;
use crate::models::{Paper, ProviderHits, SearchResult, SortOrder};
use crate::provider::{Provider, ProviderResult, SearchType};

static DOI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:https?://doi\.org/)?10\.\d{4,}/\S+$").unwrap());
static ISBN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\d{9}[\dXx]|97[89]\d{10})$").unwrap());
static AUTHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][a-z]+,\s*[A-Z]").unwrap());

pub fn detect_search_type(query: &str) -> SearchType {
    let q = query.trim();
    if DOI_RE.is_match(q) {
        return SearchType::Doi;
    }
    // ISBN check: retain only digits and X/x, then match ISBN-10 or ISBN-13
    let stripped: String = q.chars().filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x').collect();
    if ISBN_RE.is_match(&stripped) {
        return SearchType::Isbn;
    }
    if AUTHOR_RE.is_match(q) {
        return SearchType::Author;
    }
    SearchType::Keywords
}

/// Normalize a title for deduplication by stripping diacritics, punctuation, and
/// collapsing whitespace.
///
/// Uses NFKD (compatibility decomposition) intentionally rather than NFD (canonical).
/// Academic paper titles from different providers use different Unicode encodings for
/// the same content: superscripts vs digits ("x²" vs "x2"), ligatures vs separate
/// letters ("ﬁnite" vs "finite"), Roman numeral glyphs vs ASCII ("Ⅻ" vs "XII").
/// NFKD normalizes these encoding differences so deduplication works correctly.
/// NFD would miss ligature folding and encoding-variant folding, producing false
/// non-matches for papers that are genuinely the same work.
///
/// All non-alphanumeric characters (punctuation, dashes, quotes, etc.) are replaced
/// with spaces and then collapsed, so that titles differing only in punctuation style
/// (curly vs straight quotes, em-dash vs colon, trailing periods) deduplicate correctly.
/// Unicode-aware `char::is_alphanumeric()` is used, which preserves CJK, Greek,
/// Cyrillic, Devanagari, Hebrew, Arabic letters and digits.
fn normalize_title(title: &str) -> String {
    let normalized: String = title
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn deduplicate(papers: Vec<Paper>) -> Vec<Paper> {
    let mut seen_dois: HashSet<String> = HashSet::new();
    let mut seen_isbns: HashSet<String> = HashSet::new();
    let mut seen_oclcs: HashSet<String> = HashSet::new();
    let mut seen_titles: HashSet<String> = HashSet::new();
    let mut result = Vec::new();

    for paper in papers {
        // Collect keys to insert; defer insertion until all checks pass
        // so a rejected paper does not poison the seen sets.
        let mut doi_key: Option<String> = None;
        let mut isbn_key: Option<String> = None;
        let mut oclc_key: Option<String> = None;

        if let Some(doi) = &paper.doi {
            let key = doi.to_lowercase();
            if seen_dois.contains(&key) {
                continue;
            }
            doi_key = Some(key);
        }

        // ISBN and OCLC dedup only when the paper has no DOI.
        // Papers with a DOI use DOI as the authoritative identifier;
        // distinct works (e.g. book chapters) may share an ISBN.
        if paper.doi.is_none() {
            if let Some(isbn) = &paper.isbn {
                let key = isbn.replace('-', "").to_lowercase();
                if seen_isbns.contains(&key) {
                    continue;
                }
                isbn_key = Some(key);
            }

            if let Some(oclc) = &paper.oclc {
                let key = oclc.trim().to_lowercase();
                if seen_oclcs.contains(&key) {
                    continue;
                }
                oclc_key = Some(key);
            }
        }

        let norm_title = normalize_title(&paper.title);
        if !norm_title.is_empty() && seen_titles.contains(&norm_title) {
            continue;
        }

        // All checks passed — now commit all keys to the seen sets.
        if let Some(k) = doi_key {
            seen_dois.insert(k);
        }
        if let Some(k) = isbn_key {
            seen_isbns.insert(k);
        }
        if let Some(k) = oclc_key {
            seen_oclcs.insert(k);
        }
        if !norm_title.is_empty() {
            seen_titles.insert(norm_title);
        }

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

    #[test]
    fn test_deduplicate_different_dois_same_isbn_both_survive() {
        let papers = vec![
            Paper {
                title: "Chapter 1: Intro to Transformers".into(),
                doi: Some("10.1007/978-3-030-12345-6_1".into()),
                isbn: Some("978-3-030-12345-6".into()),
                ..Default::default()
            },
            Paper {
                title: "Chapter 2: Attention Mechanisms".into(),
                doi: Some("10.1007/978-3-030-12345-6_2".into()),
                isbn: Some("978-3-030-12345-6".into()),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].title, "Chapter 1: Intro to Transformers");
        assert_eq!(result[1].title, "Chapter 2: Attention Mechanisms");
    }

    #[test]
    fn test_deduplicate_no_doi_same_isbn_deduplicates() {
        let papers = vec![
            Paper {
                title: "Book From Provider A".into(),
                isbn: Some("978-3-030-12345-6".into()),
                source: "openalex".into(),
                ..Default::default()
            },
            Paper {
                title: "Book From Provider B".into(),
                isbn: Some("978-3-030-12345-6".into()),
                source: "crossref".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "openalex");
    }

    #[test]
    fn test_deduplicate_different_dois_same_oclc_both_survive() {
        let papers = vec![
            Paper {
                title: "Article in Compilation A".into(),
                doi: Some("10.1234/comp-a".into()),
                oclc: Some("987654321".into()),
                ..Default::default()
            },
            Paper {
                title: "Article in Compilation B".into(),
                doi: Some("10.1234/comp-b".into()),
                oclc: Some("987654321".into()),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_deduplicate_no_doi_same_oclc_deduplicates() {
        let papers = vec![
            Paper {
                title: "Item from source 1".into(),
                oclc: Some("987654321".into()),
                source: "openalex".into(),
                ..Default::default()
            },
            Paper {
                title: "Item from source 2".into(),
                oclc: Some("987654321".into()),
                source: "crossref".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "openalex");
    }

    #[test]
    fn test_deduplicate_mixed_doi_and_no_doi_isbn() {
        let papers = vec![
            // Two chapters with different DOIs, same ISBN -- both survive
            Paper {
                title: "Chapter 1".into(),
                doi: Some("10.1007/ch1".into()),
                isbn: Some("978-0-000-00000-0".into()),
                ..Default::default()
            },
            Paper {
                title: "Chapter 2".into(),
                doi: Some("10.1007/ch2".into()),
                isbn: Some("978-0-000-00000-0".into()),
                ..Default::default()
            },
            // A book entry with same ISBN but no DOI -- first no-DOI with this ISBN survives
            Paper {
                title: "The Whole Book".into(),
                isbn: Some("978-0-000-00000-0".into()),
                ..Default::default()
            },
            // A second no-DOI entry with the same ISBN -- should be deduped
            Paper {
                title: "The Whole Book (dup)".into(),
                isbn: Some("978-0-000-00000-0".into()),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        // Chapter 1 (DOI), Chapter 2 (DOI), The Whole Book (first no-DOI ISBN)
        // "The Whole Book (dup)" is dropped by ISBN dedup
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].title, "Chapter 1");
        assert_eq!(result[1].title, "Chapter 2");
        assert_eq!(result[2].title, "The Whole Book");
    }

    #[test]
    fn test_deduplicate_rejected_paper_does_not_poison_seen_sets() {
        // Scenario: Paper B passes DOI check (DOI is new) but fails title check
        // (title collides with Paper A). Paper B's DOI must NOT be burned —
        // Paper C, which shares B's DOI but has a unique title, must survive.
        let papers = vec![
            // Paper A: no DOI, claims title "foo"
            Paper {
                title: "Foo".into(),
                source: "provider1".into(),
                ..Default::default()
            },
            // Paper B: has DOI "10.1/x", but title "Foo" collides with A → rejected
            Paper {
                title: "Foo".into(),
                doi: Some("10.1/x".into()),
                source: "provider2".into(),
                ..Default::default()
            },
            // Paper C: same DOI "10.1/x", unique title → should survive
            Paper {
                title: "Unique Title".into(),
                doi: Some("10.1/x".into()),
                source: "provider3".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2, "Expected papers A and C to survive");
        assert_eq!(result[0].title, "Foo");
        assert_eq!(result[0].source, "provider1");
        assert_eq!(result[1].title, "Unique Title");
        assert_eq!(result[1].source, "provider3");
    }

    #[test]
    fn test_deduplicate_rejected_paper_does_not_poison_isbn() {
        // Scenario for no-DOI papers: Paper B passes ISBN check but fails title check.
        // Paper B's ISBN must not be burned.
        let papers = vec![
            // Paper A: no DOI, no ISBN, claims title "bar"
            Paper {
                title: "Bar".into(),
                source: "provider1".into(),
                ..Default::default()
            },
            // Paper B: no DOI, has ISBN "978-X", but title "Bar" collides with A → rejected
            Paper {
                title: "Bar".into(),
                isbn: Some("978-0-306-40615-7".into()),
                source: "provider2".into(),
                ..Default::default()
            },
            // Paper C: no DOI, same ISBN "978-X", unique title → should survive
            Paper {
                title: "Unique Book".into(),
                isbn: Some("978-0-306-40615-7".into()),
                source: "provider3".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2, "Expected papers A and C to survive");
        assert_eq!(result[0].title, "Bar");
        assert_eq!(result[0].source, "provider1");
        assert_eq!(result[1].title, "Unique Book");
        assert_eq!(result[1].source, "provider3");
    }

    #[test]
    fn test_detect_search_type_isbn13_spaced() {
        assert_eq!(detect_search_type("978 023117924 9"), SearchType::Isbn);
    }

    #[test]
    fn test_detect_search_type_isbn13_en_dash() {
        assert_eq!(
            detect_search_type("978\u{2013}0\u{2013}231\u{2013}17924\u{2013}9"),
            SearchType::Isbn
        );
    }

    #[test]
    fn test_isbn_search_limited_to_structured_providers() {
        use crate::provider::create_all_providers;
        let client = reqwest::Client::new();
        let config = Arc::new(Config::from_env());
        let providers = create_all_providers(client, config);

        let isbn_providers: Vec<&str> = providers
            .iter()
            .filter(|p| p.supported_search_types().contains(&SearchType::Isbn))
            .map(|p| p.name())
            .collect();

        let expected = vec!["crossref", "open_library", "google_books", "hathitrust"];
        assert_eq!(isbn_providers, expected);
    }

    #[test]
    fn test_deduplicate_rejected_paper_does_not_poison_oclc() {
        // Same pattern for OCLC: Paper B passes OCLC check but fails title check.
        let papers = vec![
            Paper {
                title: "Baz".into(),
                source: "provider1".into(),
                ..Default::default()
            },
            Paper {
                title: "Baz".into(),
                oclc: Some("99999".into()),
                source: "provider2".into(),
                ..Default::default()
            },
            Paper {
                title: "Unique Item".into(),
                oclc: Some("99999".into()),
                source: "provider3".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 2, "Expected papers A and C to survive");
        assert_eq!(result[0].title, "Baz");
        assert_eq!(result[1].title, "Unique Item");
    }

    #[test]
    fn test_normalize_title_strips_diacritics() {
        assert_eq!(normalize_title("Pāṇini"), "panini");
        assert_eq!(normalize_title("Schrödinger"), "schrodinger");
        assert_eq!(normalize_title("café résumé"), "cafe resume");
        assert_eq!(normalize_title("Ñoño François"), "nono francois");
    }

    #[test]
    fn test_normalize_title_strips_non_latin_diacritics() {
        // U+0929 DEVANAGARI LETTER NNNA decomposes (NFKD) to U+0928 + U+093C (nukta, Mn)
        // The nukta mark is outside the old 5-range hand-rolled check
        assert_eq!(normalize_title("\u{0929}"), "\u{0928}");
        // U+FB2A HEBREW LETTER SHIN WITH SHIN DOT decomposes to U+05E9 + U+05C1 (Mn)
        assert_eq!(normalize_title("\u{FB2A}"), "\u{05E9}");
        // U+0625 ARABIC LETTER ALEF WITH HAMZA BELOW decomposes to U+0627 + U+0655 (Mn)
        assert_eq!(normalize_title("\u{0625}"), "\u{0627}");
    }

    #[test]
    fn test_normalize_title_ligatures() {
        assert_eq!(normalize_title("ﬁnite ﬂow"), "finite flow");
    }

    #[test]
    fn test_normalize_title_compatibility_folding() {
        // NFKD intentionally folds superscripts, subscripts, and Roman numerals
        // so that different provider encodings of the same title deduplicate.
        // e.g. one provider returns "H₂O" and another "H2O" for the same paper.
        //
        // The "+" is non-alphanumeric and becomes a space separator under the
        // punctuation-stripping rule. This is correct: the + is irrelevant for
        // title identity; no two genuinely different papers differ only by a +
        // symbol in the title.
        assert_eq!(normalize_title("x² + y²"), "x2 y2");
        assert_eq!(normalize_title("H₂O spectroscopy"), "h2o spectroscopy");
        assert_eq!(normalize_title("Chapter Ⅻ"), "chapter xii");
        // Micro sign (U+00B5) folds to Greek mu (U+03BC) -- same symbol.
        // The hyphen is non-alphanumeric and becomes a space separator, which is
        // correct: "µ-analysis" and "µ analysis" should deduplicate as the same paper.
        assert_eq!(normalize_title("µ-analysis"), "\u{03BC} analysis");
    }

    #[test]
    fn test_normalize_title_cjk_preserved() {
        assert_eq!(normalize_title("量子計算"), "量子計算");
    }

    #[test]
    fn test_normalize_title_combined() {
        // Exercises diacritics + mixed case + whitespace + ligatures in one pass
        // to pin equivalence across allocation-reduction refactors.
        // The apostrophe is non-alphanumeric and becomes a space separator, which is
        // correct: "Schrodinger's Cat" vs "Schrodingers Cat" should deduplicate.
        assert_eq!(
            normalize_title("  Ñoño's  \u{FB01}nite  CAFÉ  "),
            "nono s finite cafe"
        );
    }

    #[test]
    fn test_normalize_title_punctuation_invariant() {
        // Trailing period stripped
        assert_eq!(
            normalize_title("Attention Is All You Need."),
            normalize_title("Attention Is All You Need"),
        );
        // Colon vs em-dash
        assert_eq!(
            normalize_title("Quantum Computing: An Introduction"),
            normalize_title("Quantum Computing \u{2014} An Introduction"),
        );
        // Curly apostrophe vs straight apostrophe
        assert_eq!(
            normalize_title("Schr\u{00F6}dinger\u{2019}s Cat"),
            normalize_title("Schrodinger's Cat"),
        );
        // En-dash vs hyphen
        assert_eq!(
            normalize_title("Semi\u{2013}supervised Learning"),
            normalize_title("Semi-supervised Learning"),
        );
        // Mixed curly quotes
        assert_eq!(
            normalize_title("\u{201C}Hello World\u{201D}"),
            normalize_title("\"Hello World\""),
        );
    }

    #[test]
    fn test_deduplicate_diacritics() {
        let papers = vec![
            Paper {
                title: "Pāṇini's Grammar".into(),
                source: "crossref".into(),
                ..Default::default()
            },
            Paper {
                title: "Panini's Grammar".into(),
                source: "openalex".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "crossref");
    }

    #[test]
    fn test_deduplicate_empty_and_diacritics_only_titles_both_kept() {
        // An all-diacritics title normalizes to "" (all combining marks stripped).
        // A genuinely empty title also normalizes to "".
        // Both should be kept: empty normalized titles must not participate in
        // title-based dedup, otherwise the first one poisons seen_titles with ""
        // and silently drops all subsequent empty-normalized papers.
        let papers = vec![
            Paper {
                title: "".into(),
                source: "provider_a".into(),
                ..Default::default()
            },
            Paper {
                title: "\u{0301}\u{0302}\u{0303}".into(),
                source: "provider_b".into(),
                ..Default::default()
            },
            Paper {
                title: "".into(),
                source: "provider_c".into(),
                ..Default::default()
            },
        ];
        let result = deduplicate(papers);
        assert_eq!(result.len(), 3);
    }
}
