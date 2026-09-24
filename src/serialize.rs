use dicom_core::{DataDictionary, Tag};
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use snafu::ResultExt;
use std::path::PathBuf;
use tracing::info;

use crate::WriteResponsesSnafu;

pub fn write_responses_to_file(
    path: PathBuf,
    response_datasets: Vec<InMemDicomObject>,
    tag_queries: Vec<Tag>,
) -> Result<(), crate::Error> {
    let dict = StandardDataDictionary;

    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_path(&path)
        .context(WriteResponsesSnafu { path: path.clone() })?;
    let header_serialized = tag_queries
        .iter()
        .map(|t| dict.by_tag(*t).unwrap().alias.to_string())
        .collect::<Vec<String>>();
    writer
        .serialize(header_serialized)
        .whatever_context("Failed serializing header row")?;

    let resp_count = response_datasets.len();
    for ds in response_datasets {
        let row: Vec<String> = tag_queries
            .iter()
            .map(|t| {
                ds.element(*t)
                    .ok()
                    .and_then(|el| el.to_str().ok())
                    .map(|cow| cow.into_owned())
                    .unwrap_or_default()
            })
            .collect();

        writer
            .serialize(row)
            .whatever_context("Failed serializing dataset")?;
    }
    writer
        .flush()
        .whatever_context("Failed to flush response file")?;
    info!("Written {resp_count} responses to `{}`", path.display());
    Ok(())
}
