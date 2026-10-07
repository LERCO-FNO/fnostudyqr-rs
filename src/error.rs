use snafu::prelude::*;
use std::path::PathBuf;

#[derive(Debug, Snafu)]
pub enum DeserError {
    #[snafu(display("Could not open {}", path.display()))]
    OpenFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("Invalid JSON in {}", path.display()))]
    Json {
        source: serde_json::Error,
        path: PathBuf,
    },

    #[snafu(display("Invalid CSV in {}", path.display()))]
    Csv { source: csv::Error, path: PathBuf },
}
