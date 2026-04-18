# research-hub

A fast CLI tool and Rust library for searching, downloading, and citing academic papers. Aggregates results from 13 scholarly providers in parallel.

## Features

- **Meta-search** across 13 providers (OpenAlex, CrossRef, PubMed, Semantic Scholar, arXiv, and more) with automatic deduplication
- **PDF download** with provider cascade -- finds and downloads full-text PDFs by DOI
- **Bibliography generation** in 6 citation formats: BibTeX, APA, MLA, Chicago, IEEE, Harvard
- **Auto-detection** of query type: DOI, author name, or keywords
- **Parallel execution** with configurable concurrency and per-provider rate limiting

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Search for papers
research-hub search "transformer attention mechanism"
research-hub search "10.1038/s41586-021-03819-2"    # DOI lookup
research-hub search "Smith, J" --limit 5             # Author search

# Download PDFs
research-hub download 10.1038/s41586-021-03819-2
research-hub download 10.1038/s41586-021-03819-2 --dir ./papers

# Batch download (DOIs as args, or a file with one DOI per line)
research-hub download-batch 10.1234/a 10.5678/b --max-concurrent 5
research-hub download-batch dois.txt

# Generate citations
research-hub bibliography 10.1038/s41586-021-03819-2 --format apa
research-hub bibliography 10.1234/a 10.5678/b --format bibtex --include-abstract
```

All commands output JSON by default.

## Providers

| Provider | Priority | Type | Search Capabilities |
|----------|----------|------|---------------------|
| OpenAlex | 180 | JSON API | DOI, keywords, author, title |
| CrossRef | 90 | JSON API | DOI, keywords, author, title |
| PubMed | 89 | JSON API | DOI, keywords, author, title |
| Semantic Scholar | 88 | JSON API | DOI, keywords, author, title |
| Unpaywall | 87 | JSON API | DOI only |
| CORE | 86 | JSON API | DOI, keywords, title |
| OpenReview | 85 | JSON API | Keywords, title |
| SSRN | 85 | HTML scrape | Keywords, title |
| arXiv | 80 | XML Atom | DOI, keywords, author, title |
| bioRxiv | 75 | JSON API | DOI only |
| MDPI | 75 | HTML scrape | Keywords, title |
| ResearchGate | 70 | HTML scrape | Keywords, title |
| Sci-Hub | 10 | HTML scrape | DOI only |

## Configuration

All settings are optional, via environment variables:

```bash
export RESEARCH_MCP_DOWNLOAD_DIR=~/papers              # Default: ~/Downloads/papers
export RESEARCH_MCP_CROSSREF_EMAIL=you@example.com      # CrossRef polite pool
export RESEARCH_MCP_SEMANTIC_SCHOLAR_API_KEY=...         # Higher rate limits
export RESEARCH_MCP_UNPAYWALL_EMAIL=you@example.com      # Required by Unpaywall API
export RESEARCH_MCP_CORE_API_KEY=...                     # CORE API access
export RESEARCH_MCP_PUBMED_API_KEY=...                   # NCBI higher rate limits
export RESEARCH_MCP_PROVIDER_TIMEOUT=30                  # Seconds per provider
export RESEARCH_MCP_MAX_PARALLEL_PROVIDERS=5             # Concurrent provider queries
```

## Library Usage

`research-hub` is also a Rust library crate:

```rust
use std::sync::Arc;
use research_hub::{Config, SearchType, create_all_providers, meta_search};

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    let client = reqwest::Client::new();
    let providers = create_all_providers(client.clone(), Arc::new(config.clone()));

    let result = meta_search("attention mechanism", &providers, &config, None, 10).await;
    println!("Found {} papers from {:?}", result.total_results, result.providers_searched);
}
```

## Development

```bash
cargo build                  # Build
cargo test                   # Run tests
cargo clippy                 # Lint
RUST_LOG=debug cargo run -- search "test"   # Run with debug logging
```

## Milestones

- [x] **v0.1.0** -- Full Rust rewrite from Python MCP server to standalone CLI + library. All 13 providers ported. 4 CLI subcommands (search, download, download-batch, bibliography). 6 citation formats. 12 unit tests passing, zero clippy warnings. ~3,800 lines of Rust.

## License

MIT
