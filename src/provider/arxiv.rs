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
    let mut extracted_arxiv_id = None;
    if let Some(caps) = arxiv_id_re.captures(&id_text) {
        let aid = caps[1].to_string();
        if doi.is_none() {
            doi = Some(format!("10.48550/arXiv.{aid}"));
        }
        extracted_arxiv_id = Some(aid);
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

    let work_type = Some(if journal.is_some() { "journal-article" } else { "preprint" }.into());

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
        arxiv_id: extracted_arxiv_id,
        work_type,
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
            SearchType::Isbn,
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
                SearchType::Keywords | SearchType::Isbn => format!("all:\"{query}\""),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a minimal Atom <entry> and parse it into a Paper.
    fn parse_entry_from_xml(xml: &str) -> Paper {
        let doc = roxmltree::Document::parse(xml).expect("valid XML");
        let entry = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name((ATOM_NS, "entry")))
            .expect("expected <entry> element");
        parse_entry(entry)
    }

    #[test]
    fn test_parse_entry_preprint_without_journal_ref() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry>
            <title>Some Preprint Title</title>
            <id>http://arxiv.org/abs/2301.00001v1</id>
            <published>2023-01-01T00:00:00Z</published>
            <summary>Abstract text</summary>
            <author><name>Alice Author</name></author>
            <link rel="alternate" href="http://arxiv.org/abs/2301.00001v1"/>
            <link type="application/pdf" href="http://arxiv.org/pdf/2301.00001v1"/>
          </entry>
        </feed>
        "#;
        let paper = parse_entry_from_xml(xml);
        assert_eq!(paper.title, "Some Preprint Title");
        assert_eq!(paper.work_type, Some("preprint".into()));
        assert!(paper.journal.is_none());
    }

    #[test]
    fn test_parse_entry_journal_article_with_journal_ref() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry>
            <title>Published Paper Title</title>
            <id>http://arxiv.org/abs/1706.03762v5</id>
            <published>2017-06-12T00:00:00Z</published>
            <summary>This paper introduces the Transformer.</summary>
            <author><name>Ashish Vaswani</name></author>
            <link rel="alternate" href="http://arxiv.org/abs/1706.03762v5"/>
            <link type="application/pdf" href="http://arxiv.org/pdf/1706.03762v5"/>
            <arxiv:journal_ref>Advances in Neural Information Processing Systems 30, 2017</arxiv:journal_ref>
          </entry>
        </feed>
        "#;
        let paper = parse_entry_from_xml(xml);
        assert_eq!(paper.title, "Published Paper Title");
        assert_eq!(
            paper.journal,
            Some("Advances in Neural Information Processing Systems 30, 2017".into())
        );
        assert_eq!(paper.work_type, Some("journal-article".into()));
    }

    #[test]
    fn test_parse_entry_journal_ref_with_doi() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry>
            <title>Paper With DOI and Journal</title>
            <id>http://arxiv.org/abs/2001.12345v2</id>
            <published>2020-01-15T00:00:00Z</published>
            <summary>Abstract</summary>
            <author><name>Bob Researcher</name></author>
            <link rel="alternate" href="http://arxiv.org/abs/2001.12345v2"/>
            <arxiv:doi>10.1234/example.2020</arxiv:doi>
            <arxiv:journal_ref>Nature 580, 123-128 (2020)</arxiv:journal_ref>
          </entry>
        </feed>
        "#;
        let paper = parse_entry_from_xml(xml);
        assert_eq!(paper.doi, Some("10.1234/example.2020".into()));
        assert_eq!(paper.journal, Some("Nature 580, 123-128 (2020)".into()));
        assert_eq!(paper.work_type, Some("journal-article".into()));
    }

    #[test]
    fn test_parse_entry_no_journal_ref_element_is_preprint() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry>
            <title>Edge Case Paper</title>
            <id>http://arxiv.org/abs/2301.99999v1</id>
            <published>2023-01-01T00:00:00Z</published>
            <summary>Abstract</summary>
            <author><name>Test Author</name></author>
            <link rel="alternate" href="http://arxiv.org/abs/2301.99999v1"/>
          </entry>
        </feed>
        "#;
        let paper = parse_entry_from_xml(xml);
        // No journal_ref element at all -> journal is None -> work_type is "preprint"
        assert!(paper.journal.is_none());
        assert_eq!(paper.work_type, Some("preprint".into()));
    }

    #[test]
    fn test_parse_entry_fields_complete() {
        // Verify other fields are correctly parsed alongside the work_type fix
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry>
            <title>  Multi  Word   Title  </title>
            <id>http://arxiv.org/abs/2301.00001v1</id>
            <published>2023-01-15T12:00:00Z</published>
            <summary>The abstract text.</summary>
            <author><name>First Author</name></author>
            <author><name>Second Author</name></author>
            <link rel="alternate" href="http://arxiv.org/abs/2301.00001v1"/>
            <link type="application/pdf" href="http://arxiv.org/pdf/2301.00001v1"/>
          </entry>
        </feed>
        "#;
        let paper = parse_entry_from_xml(xml);
        assert_eq!(paper.title, "Multi Word Title");
        assert_eq!(paper.authors, vec!["First Author", "Second Author"]);
        assert_eq!(paper.abstract_text, Some("The abstract text.".into()));
        assert_eq!(paper.published_date, Some("2023-01-15".into()));
        assert_eq!(paper.year, Some(2023));
        assert_eq!(paper.source, "arxiv");
        assert_eq!(paper.arxiv_id, Some("2301.00001".into()));
        assert_eq!(paper.doi, Some("10.48550/arXiv.2301.00001".into()));
        assert_eq!(paper.url, Some("http://arxiv.org/abs/2301.00001v1".into()));
        assert_eq!(paper.pdf_url, Some("http://arxiv.org/pdf/2301.00001v1".into()));
        assert_eq!(paper.work_type, Some("preprint".into()));
    }
}
