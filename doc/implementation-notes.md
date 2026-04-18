# Implementation Notes: Python-to-Rust Rewrite

This document captures the design decisions, porting patterns, and lessons learned during the rewrite of `research-hub-mcp-py` (Python/FastMCP) into `research-hub` (Rust CLI + library).

## Scope of the rewrite

The Python version was an MCP server exposing 4 tools to Claude Desktop/Code. The Rust version drops MCP entirely and surfaces the same functionality as a standalone CLI (`clap`) and a reusable library crate. The core domain logic -- parallel meta-search across 13 academic providers, PDF download cascade, and bibliography generation in 6 citation formats -- is ported 1:1.

## Crate layout

Single crate, `lib.rs` + `main.rs`. A workspace was considered but adds overhead for a project this size. All provider modules live under `src/provider/`; the rest (`search.rs`, `download.rs`, `bibliography.rs`, `config.rs`, `models.rs`, `error.rs`) sit directly in `src/`.

Public API is re-exported through `lib.rs`: `meta_search`, `download_paper`, `download_papers_batch`, `generate_bibliography`, plus the key types (`Config`, `Paper`, `SearchResult`, `DownloadResult`, `SearchType`, `Error`).

## Key design decisions

### Provider trait and composition

Python used class inheritance (`Provider` ABC with `self.client`, `self.config`, `self._last_request`). Rust uses a `Provider` trait (`async_trait`) plus a `ProviderBase` struct that each provider composes. The base struct holds the shared `reqwest::Client`, `Arc<Config>`, a `RateLimiter`, and an optional `base_url` override for testing.

```
Provider trait  <---  impl for OpenAlexProvider { base: ProviderBase }
```

`ProviderBase::with_base_url()` exists so integration tests can point any provider at a wiremock server without touching the trait.

### Rate limiting

Python used `time.monotonic()` with an `asyncio.sleep`. The Rust equivalent is `tokio::sync::Mutex<Instant>` -- the mutex serializes access to the last-request timestamp, and `tokio::time::sleep` handles the delay. The limiter is initialized with `Instant::now() - delay` so the first request fires immediately.

### Retry helper

The Python version used `tenacity` with decorators. Rather than pulling in `backon` or `tokio-retry`, a 15-line generic `retry()` function in `provider/mod.rs` covers the same ground: N attempts, exponential backoff (1s, 2s, 4s...) capped at 10s, with structured tracing on each failure. ResearchGate overrides to 2 attempts (matching the Python `stop_after_attempt(2)`).

### Error strategy

Provider errors during meta-search are **non-fatal** -- they get caught by the `tokio::time::timeout` wrapper in `search.rs` and accumulated into `providers_failed`. Download failures return `DownloadResult { success: false }`, not `Err`. Only infrastructure errors (`reqwest::Error`, `serde_json::Error`, `roxmltree::Error`, `std::io::Error`) propagate through `Result<T>`.

### Serde and the `abstract` field

Rust reserves `abstract` as a keyword. The `Paper` struct uses `abstract_text` internally with `#[serde(rename = "abstract")]` so the JSON output matches the Python version exactly. `PaperMetadata` (internal to bibliography) uses `abstract_text` without rename since it never serializes.

### OpenAlex inverted index

OpenAlex returns abstracts as `{"word": [pos1, pos2], ...}`. The reconstruction collects `(position, word)` pairs, sorts by position, and joins. This is identical to the Python logic. The function accepts `Option<&serde_json::Value>` and returns `Option<String>`, avoiding allocation when the field is absent.

### arXiv XML parsing

Python used `xml.etree.ElementTree` with namespace dicts. Rust uses `roxmltree` (zero-copy, no allocation for the tree). Namespace-aware lookups use `node.has_tag_name((NS, "local"))`. A helper `find_text()` searches immediate children for a given `(ns, local)` pair and returns the trimmed text.

### HTML scraping (SSRN, MDPI, ResearchGate, Sci-Hub)

Python used BeautifulSoup with the `lxml` backend. Rust uses the `scraper` crate (built on `html5ever`). CSS selectors are identical. One difference: `scraper::Selector::parse()` returns `Result`, so selectors are constructed inline with `.unwrap()` (they are compile-time constants and cannot fail in practice).

### JATS abstract stripping

CrossRef sometimes returns abstracts wrapped in `<jats:p>` XML. Both the provider and bibliography modules strip this with `scraper::Html::parse_fragment` followed by `.root_element().text().collect()`.

### Sci-Hub mirror rotation

`rand::seq::SliceRandom::choose` picks a random mirror from the 7-element static slice. User-agent rotation works the same way. The `base_url` override bypasses mirror selection for testing.

### OpenReview timestamp-to-year

OpenReview returns `cdate`/`pdate` as millisecond Unix timestamps. Rather than pulling in `chrono`, a small `chrono_lite_year()` function implements the civil-calendar year extraction from the Euclidean algorithm (days since epoch -> era -> day-of-era -> year-of-era). This avoids a heavy dependency for a single conversion.

## Bugs fixed during the port

### Borrow-after-move in `download_pdf`

The initial implementation read `content_type` from `resp.headers()` (borrowing `resp`), then called `resp.bytes().await` (consuming `resp`). Rust caught this at compile time. Fix: `.to_string()` on the content-type before consuming the response. Python's reference counting hides this class of bug.

### Dead code in biorxiv version extraction

The initial biorxiv provider had a convoluted double version-extraction block (a leftover from iterating on the type mapping between Python's dynamic `item.get("version")` and Rust's `Value` enum). The dead first block was removed, leaving a clean `Value -> String` conversion that handles both string and integer JSON values.

## What's not yet ported

- **Integration tests with wiremock**: the `base_url` override is wired up on every provider, but the `tests/` directory is empty. Next step is to add per-provider wiremock tests.
- **Smoke test**: `smoke_test.py` hits live APIs. The equivalent would be an integration test behind `#[cfg(feature = "live-tests")]`.
- **Pretty output mode**: `--output pretty` currently produces the same JSON as `--output json`. A human-readable table formatter (for search results) is a future enhancement.

## Line counts

| Module | Lines |
|--------|-------|
| `bibliography.rs` | 665 |
| `search.rs` | 223 |
| `download.rs` | 196 |
| `main.rs` | 188 |
| `provider/mod.rs` | 151 |
| 13 providers (total) | 2,239 |
| Foundation (`error`, `models`, `config`, `lib`) | 193 |
| **Total** | **3,809** |

The Python version was ~1,800 lines. The Rust version is ~2x due to explicit type handling, pattern matching on `serde_json::Value`, and the retry/rate-limit infrastructure that Python got from library decorators.
