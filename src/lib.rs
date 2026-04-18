pub mod bibliography;
pub mod config;
pub mod download;
pub mod error;
pub mod models;
pub mod provider;
pub mod search;

pub use config::Config;
pub use error::Error;
pub use models::{DownloadResult, Paper, SearchResult};
pub use provider::{Provider, ProviderResult, SearchType, create_all_providers};
pub use search::meta_search;
pub use download::{download_paper, download_papers_batch};
pub use bibliography::generate_bibliography;
