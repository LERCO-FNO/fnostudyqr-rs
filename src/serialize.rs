use dicom_core::{DataDictionary, Tag};
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use std::path::PathBuf;
use tracing::info;

pub fn write_responses_to_file(
    path: PathBuf,
    response_datasets: Vec<InMemDicomObject>,
    tag_queries: Vec<Tag>,
) -> Result<(), csv::Error> {
    let dict = StandardDataDictionary;

    let mut writer = csv::WriterBuilder::new().delimiter(b';').from_path(&path)?;
    let header_serialized = tag_queries
        .iter()
        .map(|t| dict.by_tag(*t).unwrap().alias.to_string())
        .collect::<Vec<String>>();
    writer.serialize(header_serialized)?;

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

        writer.serialize(row)?;
    }
    writer.flush()?;
    info!("Written {resp_count} responses to `{}`", path.display());
    Ok(())
}
