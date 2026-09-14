use dicom_core::{DataDictionary, Tag};
use dicom_dictionary_std::tags;
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use std::path::PathBuf;
use tracing::info;

use crate::TermQuery;

pub fn write_responses_to_file(
    path: PathBuf,
    response_datasets: Vec<InMemDicomObject>,
    other_tags: Vec<TermQuery>,
) -> Result<(), csv::Error> {
    let dict = StandardDataDictionary;

    let mut writer = csv::WriterBuilder::new().delimiter(b';').from_path(&path)?;
    let header_tags: Vec<Tag> = [tags::PATIENT_ID, tags::STUDY_INSTANCE_UID]
        .into_iter()
        .chain(other_tags.iter().map(|t| t.selector.last_tag()))
        .collect();
    let header_serialized = header_tags
        .iter()
        .map(|t| dict.by_tag(*t).unwrap().alias.to_string())
        .collect::<Vec<String>>();
    writer.serialize(header_serialized)?;

    let resp_count = response_datasets.len();
    for ds in response_datasets {
        let row: Vec<String> = header_tags
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
