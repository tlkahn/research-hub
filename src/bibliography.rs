use std::sync::Arc;

use futures::future::join_all;
use regex::Regex;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::error::Result;
use crate::models::PaperMetadata;

async fn fetch_crossref(
    client: &reqwest::Client,
    doi: &str,
    config: &Config,
) -> Option<PaperMetadata> {
    let mut params: Vec<(&str, &str)> = Vec::new();
    let email;
    if let Some(e) = &config.crossref_email {
        email = e.clone();
        params.push(("mailto", &email));
    }

    let url = format!("https://api.crossref.org/works/{doi}");
    let resp = client
        .get(&url)
        .query(&params)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }

    let data: serde_json::Value = resp.json().await.ok()?;
    let item = data.get("message")?;

    let mut authors = Vec::new();
    if let Some(arr) = item.get("author").and_then(|v| v.as_array()) {
        for a in arr {
            let given = a.get("given").and_then(|v| v.as_str()).unwrap_or("");
            let family = a.get("family").and_then(|v| v.as_str()).unwrap_or("");
            let name = format!("{given} {family}").trim().to_string();
            if !name.is_empty() {
                authors.push(name);
            }
        }
    }

    let mut year = None;
    for field in &["published-print", "published-online", "created"] {
        if let Some(parts) = item
            .get(field)
            .and_then(|v| v.get("date-parts"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_array())
            && let Some(y) = parts.first().and_then(|v| v.as_i64()) {
                year = Some(y as i32);
                break;
            }
    }

    let title = item
        .get("title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let journal = item
        .get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(String::from);

    let abstract_text = item
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(|s| {
            if s.starts_with("<jats:") || s.starts_with("<jats") {
                let fragment = scraper::Html::parse_fragment(s);
                fragment.root_element().text().collect::<String>()
            } else {
                s.to_string()
            }
        });

    Some(PaperMetadata {
        doi: doi.to_string(),
        title,
        authors,
        year,
        journal,
        volume: item.get("volume").and_then(|v| v.as_str()).map(String::from),
        issue: item.get("issue").and_then(|v| v.as_str()).map(String::from),
        pages: item.get("page").and_then(|v| v.as_str()).map(String::from),
        publisher: item
            .get("publisher")
            .and_then(|v| v.as_str())
            .map(String::from),
        abstract_text,
        url: Some(format!("https://doi.org/{doi}")),
        isbn: item
            .get("ISBN")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(String::from),
        ..Default::default()
    })
}

async fn fetch_semantic_scholar(
    client: &reqwest::Client,
    doi: &str,
    config: &Config,
) -> Option<PaperMetadata> {
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/DOI:{doi}"
    );
    let mut req = client
        .get(&url)
        .query(&[("fields", "title,authors,abstract,year,journal,externalIds")])
        .timeout(std::time::Duration::from_secs(15));

    if let Some(key) = &config.semantic_scholar_api_key {
        req = req.header("x-api-key", key);
    }

    let resp = req.send().await.ok()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }

    let data: serde_json::Value = resp.json().await.ok()?;
    let authors: Vec<String> = data
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let journal_info = data.get("journal");

    Some(PaperMetadata {
        doi: doi.to_string(),
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        authors,
        year: data.get("year").and_then(|v| v.as_i64()).map(|y| y as i32),
        journal: journal_info
            .and_then(|j| j.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from),
        volume: journal_info
            .and_then(|j| j.get("volume"))
            .and_then(|v| v.as_str())
            .map(String::from),
        pages: journal_info
            .and_then(|j| j.get("pages"))
            .and_then(|v| v.as_str())
            .map(String::from),
        abstract_text: data
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(String::from),
        url: Some(format!("https://doi.org/{doi}")),
        ..Default::default()
    })
}

pub async fn fetch_metadata(
    client: &reqwest::Client,
    doi: &str,
    config: &Config,
) -> PaperMetadata {
    if let Some(meta) = fetch_crossref(client, doi, config).await
        && !meta.title.is_empty() {
            return meta;
        }
    if let Some(meta) = fetch_semantic_scholar(client, doi, config).await
        && !meta.title.is_empty() {
            return meta;
        }
    PaperMetadata::with_fallback_title(doi)
}

fn author_last(name: &str) -> &str {
    name.split_whitespace().last().unwrap_or(name)
}

fn bibtex_key(meta: &PaperMetadata) -> String {
    let last = if meta.authors.is_empty() {
        "unknown"
    } else {
        author_last(&meta.authors[0])
    };
    let year = meta
        .year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "n.d.".into());
    let re = Regex::new(r"[^a-zA-Z]").unwrap();
    let clean = re.replace_all(last, "").to_lowercase();
    format!("{clean}{year}")
}

pub fn format_bibtex(meta: &PaperMetadata, include_abstract: bool) -> String {
    let key = bibtex_key(meta);
    let mut lines = vec![format!("@article{{{key},")];
    lines.push(format!("  title = {{{}}},", meta.title));
    if !meta.authors.is_empty() {
        lines.push(format!(
            "  author = {{{}}},",
            meta.authors.join(" and ")
        ));
    }
    if let Some(year) = meta.year {
        lines.push(format!("  year = {{{year}}},"));
    }
    if let Some(journal) = &meta.journal {
        lines.push(format!("  journal = {{{journal}}},"));
    }
    if let Some(volume) = &meta.volume {
        lines.push(format!("  volume = {{{volume}}},"));
    }
    if let Some(issue) = &meta.issue {
        lines.push(format!("  number = {{{issue}}},"));
    }
    if let Some(pages) = &meta.pages {
        lines.push(format!("  pages = {{{pages}}},"));
    }
    lines.push(format!("  doi = {{{}}},", meta.doi));
    if include_abstract
        && let Some(abs) = &meta.abstract_text {
            lines.push(format!("  abstract = {{{abs}}},"));
        }
    lines.push("}".into());
    lines.join("\n")
}

fn format_authors_apa(authors: &[String]) -> String {
    if authors.is_empty() {
        return String::new();
    }
    if authors.len() == 1 {
        let parts: Vec<&str> = authors[0].split_whitespace().collect();
        if parts.len() >= 2 {
            let initials: String = parts[..parts.len() - 1]
                .iter()
                .map(|p| format!("{}.", &p[..1]))
                .collect::<Vec<_>>()
                .join(". ");
            return format!("{}, {}", parts[parts.len() - 1], initials);
        }
        return authors[0].clone();
    }

    let mut formatted: Vec<String> = Vec::new();
    for a in authors.iter().take(19) {
        let parts: Vec<&str> = a.split_whitespace().collect();
        if parts.len() >= 2 {
            let initials: String = parts[..parts.len() - 1]
                .iter()
                .map(|p| format!("{}.", &p[..1]))
                .collect::<Vec<_>>()
                .join(". ");
            formatted.push(format!("{}, {}", parts[parts.len() - 1], initials));
        } else {
            formatted.push(a.clone());
        }
    }

    if authors.len() > 20 {
        let mut result = formatted[..19].join(", ");
        result.push_str(", ... ");
        result.push_str(formatted.last().unwrap());
        return result;
    }

    if formatted.len() > 1 {
        let last = formatted.pop().unwrap();
        let mut result = formatted.join(", ");
        result.push_str(", & ");
        result.push_str(&last);
        result
    } else {
        formatted[0].clone()
    }
}

pub fn format_apa(meta: &PaperMetadata) -> String {
    let authors = format_authors_apa(&meta.authors);
    let year = meta
        .year
        .map(|y| format!("({y})"))
        .unwrap_or_else(|| "(n.d.)".into());
    let mut parts = vec![format!("{authors} {year}. {}.", meta.title)];

    if let Some(journal) = &meta.journal {
        let mut journal_part = format!("*{journal}*");
        if let Some(volume) = &meta.volume {
            journal_part.push_str(&format!(", *{volume}*"));
        }
        if let Some(issue) = &meta.issue {
            journal_part.push_str(&format!("({issue})"));
        }
        if let Some(pages) = &meta.pages {
            journal_part.push_str(&format!(", {pages}"));
        }
        journal_part.push('.');
        parts.push(journal_part);
    }

    parts.push(format!("https://doi.org/{}", meta.doi));
    parts.join(" ")
}

pub fn format_mla(meta: &PaperMetadata) -> String {
    let first_author = if meta.authors.is_empty() {
        String::new()
    } else {
        let parts: Vec<&str> = meta.authors[0].split_whitespace().collect();
        let mut fa = if parts.len() >= 2 {
            format!(
                "{}, {}",
                parts[parts.len() - 1],
                parts[..parts.len() - 1].join(" ")
            )
        } else {
            meta.authors[0].clone()
        };
        if meta.authors.len() > 1 {
            fa.push_str(", et al");
        }
        fa
    };

    let title = format!("\"{}.\"\n", meta.title);
    let mut parts_list = vec![format!("{first_author}. {title}")];

    if let Some(journal) = &meta.journal {
        let mut j = format!("*{journal}*");
        if let Some(volume) = &meta.volume {
            j.push_str(&format!(", vol. {volume}"));
        }
        if let Some(issue) = &meta.issue {
            j.push_str(&format!(", no. {issue}"));
        }
        if let Some(year) = meta.year {
            j.push_str(&format!(", {year}"));
        }
        if let Some(pages) = &meta.pages {
            j.push_str(&format!(", pp. {pages}"));
        }
        j.push('.');
        parts_list.push(j);
    }

    parts_list.push(format!("doi:{}.", meta.doi));
    parts_list.join(" ")
}

pub fn format_chicago(meta: &PaperMetadata) -> String {
    let first_author = if meta.authors.is_empty() {
        String::new()
    } else {
        let parts: Vec<&str> = meta.authors[0].split_whitespace().collect();
        let mut fa = if parts.len() >= 2 {
            format!(
                "{}, {}",
                parts[parts.len() - 1],
                parts[..parts.len() - 1].join(" ")
            )
        } else {
            meta.authors[0].clone()
        };
        let others: Vec<_> = meta.authors[1..].to_vec();
        if !others.is_empty() {
            fa.push_str(", ");
            fa.push_str(&others.join(", "));
        }
        fa
    };

    let title = format!("\"{}.\"\n", meta.title);
    let mut result = format!("{first_author}. {title}");

    if let Some(journal) = &meta.journal {
        result.push_str(&format!(" *{journal}*"));
        if let Some(volume) = &meta.volume {
            result.push_str(&format!(" {volume}"));
        }
        if let Some(issue) = &meta.issue {
            result.push_str(&format!(", no. {issue}"));
        }
        if let Some(year) = meta.year {
            result.push_str(&format!(" ({year})"));
        }
        if let Some(pages) = &meta.pages {
            result.push_str(&format!(": {pages}"));
        }
    }

    result.push_str(&format!(". https://doi.org/{}.", meta.doi));
    result
}

pub fn format_ieee(meta: &PaperMetadata) -> String {
    let authors_str = if meta.authors.is_empty() {
        String::new()
    } else {
        let formatted: Vec<String> = meta
            .authors
            .iter()
            .map(|a| {
                let parts: Vec<&str> = a.split_whitespace().collect();
                if parts.len() >= 2 {
                    let initials: String = parts[..parts.len() - 1]
                        .iter()
                        .map(|p| format!("{}.", &p[..1]))
                        .collect::<Vec<_>>()
                        .join(". ");
                    format!("{} {}", initials, parts[parts.len() - 1])
                } else {
                    a.clone()
                }
            })
            .collect();
        formatted.join(", ")
    };

    let mut result = format!("{authors_str}, \"{},\"", meta.title);

    if let Some(journal) = &meta.journal {
        result.push_str(&format!(" *{journal}*"));
        if let Some(volume) = &meta.volume {
            result.push_str(&format!(", vol. {volume}"));
        }
        if let Some(issue) = &meta.issue {
            result.push_str(&format!(", no. {issue}"));
        }
        if let Some(pages) = &meta.pages {
            result.push_str(&format!(", pp. {pages}"));
        }
        if let Some(year) = meta.year {
            result.push_str(&format!(", {year}"));
        }
    }

    result.push_str(&format!(". doi: {}.", meta.doi));
    result
}

pub fn format_harvard(meta: &PaperMetadata) -> String {
    let first_author = if meta.authors.is_empty() {
        String::new()
    } else {
        let parts: Vec<&str> = meta.authors[0].split_whitespace().collect();
        let mut fa = if parts.len() >= 2 {
            let initials: String = parts[..parts.len() - 1]
                .iter()
                .map(|p| format!("{}.", &p[..1]))
                .collect::<Vec<_>>()
                .join(". ");
            format!("{}, {}", parts[parts.len() - 1], initials)
        } else {
            meta.authors[0].clone()
        };

        if meta.authors.len() > 1 {
            let others: Vec<String> = meta.authors[1..]
                .iter()
                .map(|a| {
                    let p: Vec<&str> = a.split_whitespace().collect();
                    if p.len() >= 2 {
                        let initials: String = p[..p.len() - 1]
                            .iter()
                            .map(|x| format!("{}.", &x[..1]))
                            .collect::<Vec<_>>()
                            .join(". ");
                        format!("{} {}", initials, p[p.len() - 1])
                    } else {
                        a.clone()
                    }
                })
                .collect();
            fa.push_str(", ");
            fa.push_str(&others.join(" and "));
        }
        fa
    };

    let year = meta
        .year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "n.d.".into());
    let mut result = format!("{first_author} ({year}) '{}',", meta.title);

    if let Some(journal) = &meta.journal {
        result.push_str(&format!(" *{journal}*"));
        if let Some(volume) = &meta.volume {
            result.push_str(&format!(", {volume}"));
        }
        if let Some(issue) = &meta.issue {
            result.push_str(&format!("({issue})"));
        }
        if let Some(pages) = &meta.pages {
            result.push_str(&format!(", pp. {pages}"));
        }
    }

    result.push_str(&format!(". doi: {}.", meta.doi));
    result
}

pub async fn generate_bibliography(
    identifiers: &[String],
    client: &reqwest::Client,
    config: &Config,
    fmt: &str,
    include_abstract: bool,
) -> Result<serde_json::Value> {
    let fmt_lower = fmt.to_lowercase();
    let valid_formats = [
        "bibtex", "apa", "mla", "chicago", "ieee", "harvard",
    ];
    if !valid_formats.contains(&fmt_lower.as_str()) {
        return Ok(serde_json::json!({
            "error": format!("Unknown format '{fmt}'. Supported: {}", valid_formats.join(", ")),
            "entries": [],
        }));
    }

    let sem = Arc::new(Semaphore::new(30));

    let tasks: Vec<_> = identifiers
        .iter()
        .map(|id| {
            let sem = sem.clone();
            let client = client.clone();
            let config = config.clone();
            let doi = id.trim_start_matches("https://doi.org/").to_string();
            async move {
                let _permit = sem.acquire().await.unwrap();
                let meta = fetch_metadata(&client, &doi, &config).await;
                (doi, meta)
            }
        })
        .collect();

    let results = join_all(tasks).await;

    let mut entries = Vec::new();
    let mut errors = Vec::new();

    for (doi, meta) in results {
        if meta.title.starts_with("Unknown paper") {
            errors.push(format!("Could not find metadata for {doi}"));
            continue;
        }

        let entry = match fmt_lower.as_str() {
            "bibtex" => format_bibtex(&meta, include_abstract),
            "apa" => format_apa(&meta),
            "mla" => format_mla(&meta),
            "chicago" => format_chicago(&meta),
            "ieee" => format_ieee(&meta),
            "harvard" => format_harvard(&meta),
            _ => unreachable!(),
        };
        entries.push(entry);
    }

    Ok(serde_json::json!({
        "format": fmt,
        "entries": entries,
        "bibliography": entries.join("\n\n"),
        "count": entries.len(),
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> PaperMetadata {
        PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Test Paper Title".into(),
            authors: vec!["John Smith".into(), "Jane Doe".into()],
            year: Some(2024),
            journal: Some("Nature".into()),
            volume: Some("42".into()),
            issue: Some("3".into()),
            pages: Some("100-110".into()),
            publisher: Some("Springer".into()),
            abstract_text: Some("Test abstract.".into()),
            url: Some("https://doi.org/10.1234/test".into()),
            ..Default::default()
        }
    }

    #[test]
    fn test_format_bibtex() {
        let meta = sample_meta();
        let result = format_bibtex(&meta, false);
        assert!(result.starts_with("@article{smith2024,"));
        assert!(result.contains("title = {Test Paper Title}"));
        assert!(result.contains("author = {John Smith and Jane Doe}"));
        assert!(result.contains("year = {2024}"));
        assert!(!result.contains("abstract"));
    }

    #[test]
    fn test_format_bibtex_with_abstract() {
        let meta = sample_meta();
        let result = format_bibtex(&meta, true);
        assert!(result.contains("abstract = {Test abstract.}"));
    }

    #[test]
    fn test_format_apa() {
        let meta = sample_meta();
        let result = format_apa(&meta);
        assert!(result.contains("Smith, J., & Doe, J."));
        assert!(result.contains("(2024)"));
        assert!(result.contains("*Nature*"));
    }

    #[test]
    fn test_format_mla() {
        let meta = sample_meta();
        let result = format_mla(&meta);
        assert!(result.contains("Smith, John"));
        assert!(result.contains("et al"));
        assert!(result.contains("*Nature*"));
    }

    #[test]
    fn test_format_ieee() {
        let meta = sample_meta();
        let result = format_ieee(&meta);
        assert!(result.contains("J. Smith"));
        assert!(result.contains("J. Doe"));
        assert!(result.contains("*Nature*"));
    }

    #[test]
    fn test_format_harvard() {
        let meta = sample_meta();
        let result = format_harvard(&meta);
        assert!(result.contains("Smith, J."));
        assert!(result.contains("(2024)"));
        assert!(result.contains("*Nature*"));
    }

    #[test]
    fn test_format_chicago() {
        let meta = sample_meta();
        let result = format_chicago(&meta);
        assert!(result.contains("Smith, John"));
        assert!(result.contains("Jane Doe"));
        assert!(result.contains("*Nature*"));
        assert!(result.contains("(2024)"));
        assert!(result.contains("https://doi.org/10.1234/test"));
    }

    #[test]
    fn test_format_chicago_no_authors() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Orphan Paper".into(),
            ..Default::default()
        };
        let result = format_chicago(&meta);
        assert!(result.contains("Orphan Paper"));
        assert!(result.contains("https://doi.org/10.1234/test"));
    }

    #[test]
    fn test_bibtex_key_basic() {
        let meta = sample_meta();
        assert_eq!(bibtex_key(&meta), "smith2024");
    }

    #[test]
    fn test_bibtex_key_no_authors() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            year: Some(2020),
            ..Default::default()
        };
        assert_eq!(bibtex_key(&meta), "unknown2020");
    }

    #[test]
    fn test_bibtex_key_no_year() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            authors: vec!["Alice Johnson".into()],
            ..Default::default()
        };
        assert_eq!(bibtex_key(&meta), "johnsonn.d.");
    }

    #[test]
    fn test_author_last_single_name() {
        assert_eq!(author_last("Aristotle"), "Aristotle");
    }

    #[test]
    fn test_author_last_full_name() {
        assert_eq!(author_last("John Smith"), "Smith");
    }

    #[test]
    fn test_author_last_three_parts() {
        assert_eq!(author_last("Mary Jane Watson"), "Watson");
    }

    #[test]
    fn test_format_authors_apa_empty() {
        assert_eq!(format_authors_apa(&[]), "");
    }

    #[test]
    fn test_format_authors_apa_single() {
        let authors = vec!["John Smith".to_string()];
        let result = format_authors_apa(&authors);
        assert_eq!(result, "Smith, J.");
    }

    #[test]
    fn test_format_authors_apa_two() {
        let authors = vec!["John Smith".to_string(), "Jane Doe".to_string()];
        let result = format_authors_apa(&authors);
        assert!(result.contains("Smith, J."));
        assert!(result.contains("& Doe, J."));
    }

    #[test]
    fn test_format_authors_apa_single_word_name() {
        let authors = vec!["Madonna".to_string()];
        let result = format_authors_apa(&authors);
        assert_eq!(result, "Madonna");
    }

    #[test]
    fn test_format_bibtex_minimal() {
        let meta = PaperMetadata {
            doi: "10.1234/minimal".into(),
            title: "Minimal".into(),
            ..Default::default()
        };
        let result = format_bibtex(&meta, false);
        assert!(result.contains("@article{unknownn.d.,"));
        assert!(result.contains("title = {Minimal}"));
        assert!(result.contains("doi = {10.1234/minimal}"));
        assert!(!result.contains("author"));
        assert!(!result.contains("year"));
    }

    #[test]
    fn test_format_apa_no_journal() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Some Title".into(),
            authors: vec!["John Smith".into()],
            year: Some(2023),
            ..Default::default()
        };
        let result = format_apa(&meta);
        assert!(result.contains("Smith, J. (2023). Some Title."));
        assert!(!result.contains("*"));
    }

    #[test]
    fn test_format_apa_no_year() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Title".into(),
            authors: vec!["John Smith".into()],
            ..Default::default()
        };
        let result = format_apa(&meta);
        assert!(result.contains("(n.d.)"));
    }

    #[test]
    fn test_format_mla_single_author() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Title".into(),
            authors: vec!["John Smith".into()],
            year: Some(2024),
            journal: Some("Science".into()),
            ..Default::default()
        };
        let result = format_mla(&meta);
        assert!(result.contains("Smith, John"));
        assert!(!result.contains("et al"));
    }

    #[test]
    fn test_format_ieee_no_authors() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Solo".into(),
            ..Default::default()
        };
        let result = format_ieee(&meta);
        assert!(result.contains("\"Solo,\""));
    }

    #[test]
    fn test_format_harvard_no_year() {
        let meta = PaperMetadata {
            doi: "10.1234/test".into(),
            title: "Title".into(),
            authors: vec!["A B".into()],
            ..Default::default()
        };
        let result = format_harvard(&meta);
        assert!(result.contains("(n.d.)"));
    }

    #[test]
    fn test_sample_meta_has_no_isbn_by_default() {
        let meta = sample_meta();
        // sample_meta represents a journal article, so isbn/oclc/lccn should be None
        assert_eq!(meta.isbn, None);
        assert_eq!(meta.oclc, None);
        assert_eq!(meta.lccn, None);
    }

    #[test]
    fn test_paper_metadata_isbn_preserved() {
        let meta = PaperMetadata {
            doi: "10.1007/978-3-030-12345-6_1".into(),
            title: "A Book Chapter".into(),
            authors: vec!["John Smith".into()],
            year: Some(2024),
            isbn: Some("978-3-030-12345-6".into()),
            ..Default::default()
        };
        assert_eq!(meta.isbn, Some("978-3-030-12345-6".into()));
        // Verify it works with BibTeX formatter (isbn should not crash anything)
        let bibtex = format_bibtex(&meta, false);
        assert!(bibtex.contains("title = {A Book Chapter}"));
    }
}
