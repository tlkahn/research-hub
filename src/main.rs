use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};

use research_hub::SortOrder;
use research_hub::config::Config;
use research_hub::provider::create_all_providers;

#[derive(Parser)]
#[command(name = "research-hub", about = "Search, download, and cite academic papers")]
struct Cli {
    #[arg(long, default_value = "json", global = true)]
    output: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Pretty,
}

#[derive(Subcommand)]
enum Commands {
    /// Search for academic papers across multiple providers
    Search {
        /// Search query (DOI, author name, or keywords)
        query: String,
        /// Maximum number of results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Number of results to skip (for pagination)
        #[arg(short, long, default_value_t = 0)]
        offset: usize,
        /// Sort order: relevance, date, date-asc, citations
        #[arg(short, long, default_value = "relevance")]
        sort: SortOrder,
        /// Only search these providers (comma-separated or repeated)
        #[arg(short, long, value_delimiter = ',')]
        provider: Vec<String>,
        /// Exclude these providers (comma-separated or repeated)
        #[arg(short = 'x', long, value_delimiter = ',')]
        exclude_provider: Vec<String>,
    },
    /// Download a paper PDF by DOI
    Download {
        /// DOI of the paper
        doi: String,
        /// Download directory
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
    /// Download multiple papers by DOI
    DownloadBatch {
        /// DOIs (positional args or path to file with one DOI per line)
        dois: Vec<String>,
        /// Maximum concurrent downloads
        #[arg(short, long, default_value_t = 9)]
        max_concurrent: usize,
        /// Download directory
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
    /// Generate bibliography entries for papers
    Bibliography {
        /// DOIs or doi.org URLs
        dois: Vec<String>,
        /// Citation format
        #[arg(short, long, default_value = "bibtex")]
        format: String,
        /// Include abstracts (BibTeX only)
        #[arg(long, name = "abstract")]
        include_abstract: bool,
    },
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .gzip(true)
        .build()
        .expect("failed to build HTTP client")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::from_env();
    let client = build_client();
    let providers = create_all_providers(client.clone(), Arc::new(config.clone()));

    match cli.command {
        Commands::Search {
            query,
            limit,
            offset,
            sort,
            provider: include,
            exclude_provider: exclude,
        } => {
            let filtered: Vec<_> = providers
                .iter()
                .filter(|p| {
                    let name = p.name();
                    (include.is_empty() || include.iter().any(|i| i.eq_ignore_ascii_case(name)))
                        && !exclude.iter().any(|e| e.eq_ignore_ascii_case(name))
                })
                .cloned()
                .collect();
            let result =
                research_hub::meta_search(&query, &filtered, &config, None, limit, offset, sort)
                    .await;
            print_output(&cli.output, &result);
        }
        Commands::Download { doi, dir } => {
            let result = research_hub::download_paper(
                &doi,
                &providers,
                &client,
                &config,
                dir.as_deref(),
            )
            .await;
            print_output(&cli.output, &result);
        }
        Commands::DownloadBatch {
            dois,
            max_concurrent,
            dir,
        } => {
            let specs = resolve_dois(dois).await;
            let paper_specs: Vec<serde_json::Value> = specs
                .iter()
                .map(|doi| serde_json::json!({"doi": doi}))
                .collect();

            let results = research_hub::download_papers_batch(
                &paper_specs,
                &providers,
                &client,
                &config,
                max_concurrent,
                dir.as_deref(),
            )
            .await;

            let output = serde_json::json!({
                "results": results,
                "total": results.len(),
            });
            print_output(&cli.output, &output);
        }
        Commands::Bibliography {
            dois,
            format,
            include_abstract,
        } => {
            let identifiers: Vec<String> = dois;
            match research_hub::generate_bibliography(
                &identifiers,
                &client,
                &config,
                &format,
                include_abstract,
            )
            .await
            {
                Ok(result) => print_output(&cli.output, &result),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

async fn resolve_dois(inputs: Vec<String>) -> Vec<String> {
    if inputs.len() == 1 {
        let path = PathBuf::from(&inputs[0]);
        if path.exists() && path.is_file()
            && let Ok(content) = tokio::fs::read_to_string(&path).await {
                return content
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
            }
    }
    inputs
}

fn print_output(format: &OutputFormat, value: &impl serde::Serialize) {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("Error: {e}"))
            );
        }
        OutputFormat::Pretty => {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("Error: {e}"))
            );
        }
    }
}
