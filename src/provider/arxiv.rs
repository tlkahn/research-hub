use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, ProviderResult, SearchType, retry};

const ATOM_NS: &str = "http://www.w3.org/2005/Atom";
const ARXIV_NS: &str = "http://arxiv.org/schemas/atom";
const OPENSEARCH_NS: &str = "http://a9.com/-/spec/opensearch/1.1/";

fn find_text<'a>(node: roxmltree::Node<'a, 'a>, ns: &str, local: &str) -> Option<String> {
    node.children()
        .find(|c| c.has_tag_name((ns, local)))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
}

fn parse_entry(entry: roxmltree::Node) -> Paper {
    let title = find_text(entry, ATOM_NS, "title")
        .unwrap_or_default()
        .replace('\n', " ");
    let title = regex::Regex::new(r"\s+")
        .unwrap()
        .replace_all(&title, " ")
        .trim()
        .to_string();

    let abstract_text = find_text(entry, ATOM_NS, "summary");

    let mut authors = Vec::new();
    for child in entry.children() {
        if child.has_tag_name((ATOM_NS, "author"))
            && let Some(name) = find_text(child, ATOM_NS, "name") {
                authors.push(name);
            }
    }

    let mut url = String::new();
    let mut pdf_url = None;
    for child in entry.children() {
        if child.has_tag_name((ATOM_NS, "link")) {
            let rel = child.attribute("rel").unwrap_or("");
            let href = child.attribute("href").unwrap_or("");
            let link_type = child.attribute("type").unwrap_or("");
            if rel == "alternate" {
                url = href.to_string();
            } else if link_type == "application/pdf"
                || (rel == "related" && href.contains("pdf"))
            {
                pdf_url = Some(href.to_string());
            }
        }
    }

    let mut doi = None;
    for child in entry.children() {
        if child.has_tag_name((ARXIV_NS, "doi"))
            && let Some(text) = child.text() {
                doi = Some(text.trim().to_string());
            }
    }

    let id_text = find_text(entry, ATOM_NS, "id").unwrap_or_default();
    let arxiv_id_re = Regex::new(r"abs/(.+?)(?:v\d+)?$").unwrap();
    if let Some(caps) = arxiv_id_re.captures(&id_text) {
        let arxiv_id = &caps[1];
        if doi.is_none() {
            doi = Some(format!("10.48550/arXiv.{arxiv_id}"));
        }
    }

    let published_raw = find_text(entry, ATOM_NS, "published");
    let published_date = published_raw
        .as_deref()
        .and_then(|s| s.get(..10))
        .map(String::from);
    let year = published_raw
        .as_deref()
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<i32>().ok());

    let mut journal = None;
    for child in entry.children() {
        if child.has_tag_name((ARXIV_NS, "journal_ref"))
            && let Some(text) = child.text() {
                journal = Some(text.trim().to_string());
            }
    }

    Paper {
        title,
        authors,
        abstract_text,
        doi,
        year,
        published_date,
        source: "arxiv".into(),
        url: if url.is_empty() { None } else { Some(url) },
        pdf_url,
        journal,
        ..Default::default()
    }
}

pub struct ArxivProvider {
    base: ProviderBase,
}

impl ArxivProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_secs(3)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://export.arxiv.org/api/query")
    }
}

#[async_trait]
impl Provider for ArxivProvider {
    fn name(&self) -> &str {
        "arxiv"
    }
    fn priority(&self) -> i32 {
        80
    }
    fn base_delay(&self) -> Duration {
        Duration::from_secs(3)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[
            SearchType::Keywords,
            SearchType::Doi,
            SearchType::Author,
            SearchType::Title,
        ]
    }

    async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
        offset: usize,
    ) -> Result<ProviderResult> {
        let base = &self.base;
        retry("arxiv", 3, || async {
            base.rate_limiter.wait().await;

            let search_query = match search_type {
                SearchType::Doi => {
                    let stripped = query.trim_start_matches("https://doi.org/");
                    let arxiv_re = Regex::new(r"arXiv\.(.+)").unwrap();
                    if let Some(caps) = arxiv_re.captures(stripped) {
                        format!("id:{}", &caps[1])
                    } else {
                        format!("all:\"{query}\"")
                    }
                }
                SearchType::Author => format!("au:\"{query}\""),
                SearchType::Title => format!("ti:\"{query}\""),
                SearchType::Keywords => format!("all:\"{query}\""),
            };

            let resp = base
                .client
                .get(self.base_url())
                .query(&[
                    ("search_query", search_query.as_str()),
                    ("start", &offset.to_string()),
                    ("max_results", &limit.to_string()),
                    ("sortBy", "relevance"),
                    ("sortOrder", "descending"),
                ])
                .send()
                .await?;
            resp.error_for_status_ref()?;

            let text = resp.text().await?;
            let doc = roxmltree::Document::parse(&text)?;
            let root = doc.root_element();

            let total_hits = find_text(root, OPENSEARCH_NS, "totalResults")
                .and_then(|s| s.parse::<usize>().ok());

            let papers: Vec<Paper> = root
                .children()
                .filter(|n| n.has_tag_name((ATOM_NS, "entry")))
                .take(limit)
                .map(parse_entry)
                .collect();
            Ok(ProviderResult { papers, total_hits })
        })
        .await
    }
}
