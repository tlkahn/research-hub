use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use scraper::{Html, Selector};

use crate::config::Config;
use crate::error::Result;
use crate::models::Paper;
use crate::provider::{Provider, ProviderBase, SearchType, retry};

pub struct MdpiProvider {
    base: ProviderBase,
}

impl MdpiProvider {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self {
            base: ProviderBase::new(client, config, Duration::from_secs(1)),
        }
    }

    fn base_url(&self) -> &str {
        self.base
            .base_url
            .as_deref()
            .unwrap_or("https://www.mdpi.com")
    }
}

#[async_trait]
impl Provider for MdpiProvider {
    fn name(&self) -> &str {
        "mdpi"
    }
    fn priority(&self) -> i32 {
        75
    }
    fn base_delay(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn supported_search_types(&self) -> &[SearchType] {
        &[SearchType::Keywords, SearchType::Title]
    }

    async fn search(
        &self,
        query: &str,
        _search_type: SearchType,
        limit: usize,
    ) -> Result<Vec<Paper>> {
        let base = &self.base;
        retry("mdpi", 3, || async {
            base.rate_limiter.wait().await;

            let url = format!("{}/search", self.base_url());
            let resp = base
                .client
                .get(&url)
                .query(&[("q", query), ("page_count", &limit.to_string())])
                .header("User-Agent", "Mozilla/5.0 (compatible; research-bot)")
                .send()
                .await?;
            resp.error_for_status_ref()?;

            let html = resp.text().await?;
            let document = Html::parse_document(&html);
            let article_sel =
                Selector::parse(".article-content, .generic-item").unwrap();
            let title_sel =
                Selector::parse("a.title-is-link, .title a").unwrap();
            let author_sel =
                Selector::parse(".authors a, .author-name").unwrap();
            let abstract_sel =
                Selector::parse(".abstract-full, .abstract").unwrap();
            let doi_re = Regex::new(r"mdpi\.com/(\d+-\d+/\d+/\d+/\d+)").unwrap();

            let mut papers = Vec::new();
            for article in document.select(&article_sel).take(limit) {
                let title_el = match article.select(&title_sel).next() {
                    Some(el) => el,
                    None => continue,
                };

                let title: String = title_el.text().collect::<String>().trim().to_string();
                let mut href = title_el
                    .value()
                    .attr("href")
                    .unwrap_or("")
                    .to_string();
                if !href.is_empty() && !href.starts_with("http") {
                    href = format!("{}{}", self.base_url(), href);
                }

                let authors: Vec<String> = article
                    .select(&author_sel)
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                let abstract_text = article
                    .select(&abstract_sel)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string());

                let doi = doi_re.captures(&href).map(|c| {
                    let path = &c[1];
                    format!("10.3390/{}", path.replace('/', ""))
                });

                let pdf_url = if !href.is_empty() {
                    Some(format!("{href}/pdf"))
                } else {
                    None
                };

                papers.push(Paper {
                    title,
                    authors,
                    abstract_text,
                    doi,
                    source: "mdpi".into(),
                    url: Some(href),
                    pdf_url,
                    ..Default::default()
                });
            }
            Ok(papers)
        })
        .await
    }
}
