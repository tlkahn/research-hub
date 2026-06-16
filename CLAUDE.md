# CLAUDE.md

## What This Is

`research-hub` -- a Rust CLI tool and library for searching, downloading, and citing academic papers. Aggregates results from 13 providers via parallel meta-search. Rewritten from `research-hub-mcp-py` (Python/FastMCP); MCP has been removed.

## Commands

```bash
cargo build                              # Build
cargo test                               # Run all tests (125 unit tests)
cargo clippy                             # Lint (must be warning-free)
cargo run -- search "transformer"        # Search papers
cargo run -- download 10.1234/test       # Download PDF by DOI
cargo run -- bibliography 10.1234/test --format apa  # Generate citation
```

## Architecture

### Request Flow

```
CLI (clap, main.rs)
  -> search.rs / download.rs / bibliography.rs
    -> provider/ (13 providers, parallel via tokio + futures::join_all)
      -> External APIs / HTML scraping
```

### Key Modules

- **`main.rs`** (218 lines) -- CLI entry point. 4 subcommands: `search`, `download`, `download-batch`, `bibliography`.
- **`lib.rs`** (15 lines) -- Public API re-exports: `meta_search`, `download_paper`, `download_papers_batch`, `generate_bibliography`, plus key types.
- **`provider/mod.rs`** (364 lines) -- `Provider` trait, `SearchType` enum (Doi, Keywords, Author, Title, Isbn), `ProviderResult` (incl. `total_hits`), `RateLimiter`, `ProviderBase` (shared client + config), generic `retry()` helper, `create_all_providers()`.
- **`provider/*.rs`** -- 13 provider implementations (~130-225 lines each). Each composes a `ProviderBase` and implements the `Provider` trait. All accept optional `base_url` override for testing.
- **`search.rs`** (730 lines) -- `detect_search_type()` (DOI regex, ISBN regex after hyphen-stripping for ISBN-10/ISBN-13, author pattern, fallback keywords), `deduplicate()` (by DOI > ISBN > OCLC > normalized title; ISBN/OCLC only checked when paper has no DOI), `meta_search()` (semaphore-limited parallel queries with per-provider timeout, offset pagination, sort by date/citations).
- **`download.rs`** (323 lines) -- `download_paper()`: search for pdf_url -> provider cascade by priority. `download_papers_batch()`: concurrent with semaphore. Validates PDF via content-type and magic bytes.
- **`bibliography.rs`** (868 lines) -- `fetch_metadata()` (CrossRef primary, Semantic Scholar fallback), 6 formatters: `format_bibtex`, `format_apa`, `format_mla`, `format_chicago`, `format_ieee`, `format_harvard`.
- **`models.rs`** (447 lines) -- `Paper` (23 fields incl. `published_date`, `abstract` renamed to `abstract_text`, plus `publisher`, `isbn`, `issn`, `arxiv_id`, `work_type`, `editors`, `series`, `oclc`, `lccn`), `SortOrder` enum, `SearchResult` (incl. `offset`/`sort`/`total_hits`/`provider_hits`), `ProviderHits`, `DownloadResult`, `PaperMetadata`.
- **`config.rs`** (113 lines) -- 8 env vars (`RESEARCH_MCP_*`), no required vars.
- **`error.rs`** (81 lines) -- `Error` enum with 8 variants (Http, Json, Xml, Io, Provider, UnknownFormat, NoPdf, Timeout), `thiserror` derives. Provider errors are non-fatal during search.

### Provider Priorities

200+ authoritative, 100-149 specialized, 50-99 general, 0-49 fallback. Current: OpenAlex (180), CrossRef (90), PubMed (89), Semantic Scholar (88), Unpaywall (87), CORE (86), OpenReview (85), SSRN (85), arXiv (80), bioRxiv (75), MDPI (75), ResearchGate (70), Sci-Hub (10).

### Providers by API type

- **JSON API**: OpenAlex, CrossRef, PubMed (two-stage: esearch+esummary), Semantic Scholar, Unpaywall, CORE, OpenReview, bioRxiv
- **XML Atom**: arXiv (roxmltree, namespace-aware)
- **HTML scrape**: SSRN, MDPI, ResearchGate (Chrome UA), Sci-Hub (7 mirrors, embed/iframe/regex PDF extraction)

## Code Patterns

- All providers share one `reqwest::Client` (connection pooling, gzip, rustls)
- `ProviderBase` composes client + config + rate limiter; each provider struct wraps it
- `retry()` in `provider/mod.rs`: generic, 3 attempts default, exponential backoff capped at 10s
- `RateLimiter`: `tokio::sync::Mutex<Instant>` + per-provider delay
- `serde_json::Value` used for JSON parsing (no per-provider response structs)
- `scraper` crate for HTML parsing and JATS stripping
- Providers stored as `Vec<Arc<dyn Provider>>`, sorted by descending priority
- `Reverse(priority)` via `sort_by_key` for descending sort

## Config (env vars)

| Var | Default | Notes |
|-----|---------|-------|
| `RESEARCH_MCP_DOWNLOAD_DIR` | `~/Downloads/papers` | PDF destination |
| `RESEARCH_MCP_CROSSREF_EMAIL` | None | Polite pool |
| `RESEARCH_MCP_SEMANTIC_SCHOLAR_API_KEY` | None | x-api-key header |
| `RESEARCH_MCP_UNPAYWALL_EMAIL` | `user@example.com` | Required param |
| `RESEARCH_MCP_CORE_API_KEY` | None | Bearer token |
| `RESEARCH_MCP_PUBMED_API_KEY` | None | NCBI api_key |
| `RESEARCH_MCP_PROVIDER_TIMEOUT` | 30s | Per-provider timeout |
| `RESEARCH_MCP_MAX_PARALLEL_PROVIDERS` | 5 | Semaphore permits |

## Dependencies (14 runtime, 3 dev)

Runtime: `tokio`, `futures`, `reqwest` (rustls-tls, gzip), `serde`, `serde_json`, `roxmltree`, `scraper`, `clap`, `thiserror`, `regex`, `async-trait`, `rand`, `tracing`, `tracing-subscriber`.

Dev: `wiremock`, `tokio-test`, `tempfile`.

## Testing

- 125 unit tests (in-module `#[cfg(test)]`): `models::tests` (Paper serde, defaults, new fields), `search::tests` (detect_search_type incl. ISBN, normalize_title, deduplicate incl. ISBN/OCLC chains), `bibliography::tests` (all 6 formatters), `download::tests`, `provider::tests` (SearchType serde, provider ordering, rate limiter, retry, ISBN support), `provider::arxiv`, `provider::crossref`, `config::tests`, `error::tests`
- Integration tests with wiremock: not yet written (`tests/` dir exists, `base_url` override is wired up on all providers)
- Logging: `RUST_LOG=debug cargo run -- ...` for provider-level tracing

## Known Gaps

- `--output pretty` mode produces JSON (same as `--output json`); human-readable tables not yet implemented
- No integration tests yet (wiremock infrastructure is ready)
- No smoke test against live APIs yet
