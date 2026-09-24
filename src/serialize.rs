use dicom_core::{DataDictionary, DataElement, Tag};
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use std::path::PathBuf;
use tracing::info;

use dicom_core::VR::*;

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

#[derive(Debug, Serialize, Deserialize)]
enum ValueType {
    SignedInteger(i32),
    UnsignedInteger(u32),
    Float(f64),
    Text(String),
    AgeString(String),
    Error(DicomValueParseError),
}

#[derive(Serialize)]
struct TagData {
    header: Vec<String>,
    values: Vec<Vec<ValueType>>,
}

#[derive(Debug, Clone, Copy)]
enum TagType {
    Keyword,
    Hex,
}

#[derive(Debug, Serialize, Deserialize)]
enum DicomValueParseError {
    MissingTag,
    UnknownVR,
    InvalidSignedInteger,
    InvalidFloat,
    InvalidUnsignedInteger,
    InvalidDate,
    InvalidTime,
    InvalidDateTime,
    InvalidString,
    InvalidAgeString,
}

fn parse_age(element: &DataElement<InMemDicomObject>) -> Option<u32> {
    element.to_str().ok().and_then(|s| {
        let trimmed = s.trim();

        if trimmed.len() != 4 {
            return None;
        }

        // split at 4th character (3rd index)
        let (num_part, _) = trimmed.split_at(3);

        num_part.parse::<u32>().ok()
    })
}

fn extract_tag_values(tags: &[Tag], ds: &InMemDicomObject) -> Vec<ValueType> {
    tags.iter()
        .map(|t| match ds.element(*t) {
            Ok(element) => parse_element_value(element).unwrap_or_else(ValueType::Error),
            Err(_) => ValueType::Error(DicomValueParseError::MissingTag),
        })
        .collect()
}

fn parse_element_value(
    element: &DataElement<InMemDicomObject>,
) -> Result<ValueType, DicomValueParseError> {
    type V = ValueType;
    type E = DicomValueParseError;
    match element.vr() {
        AE | AS | CS | LO | LT | PN | SH | ST | UI | UR | UT => element
            .to_str()
            .map(|s| V::Text(s.to_string()))
            .map_err(|_| E::InvalidString),
        IS | SS | SL => element
            .to_int::<i32>()
            .map(V::SignedInteger)
            .map_err(|_| E::InvalidSignedInteger),
        US | UL => element
            .to_int::<u32>()
            .map(V::UnsignedInteger)
            .map_err(|_| E::InvalidUnsignedInteger),
        DS | FL | FD => element
            .to_float64()
            .map(V::Float)
            .map_err(|_| E::InvalidFloat),
        DA => element
            .to_date()
            .map(|d| V::Text(d.to_string()))
            .map_err(|_| E::InvalidDate),
        TM => element
            .to_time()
            .map(|t| V::Text(t.to_string()))
            .map_err(|_| E::InvalidTime),
        DT => element
            .to_datetime()
            .map(|dt| V::Text(dt.to_string()))
            .map_err(|_| E::InvalidDateTime),
        _ => Err(E::UnknownVR),
    }
}

#[cfg(test)]
mod tests {
    use dicom_core::{DataElement, VR};
    use dicom_dictionary_std::tags;

    use super::*;

    #[test]
    fn serialize_json() {
        let datasets = [
            InMemDicomObject::from_element_iter([
                DataElement::new(tags::PATIENT_ID, VR::PN, "0123"),
                DataElement::new(tags::PATIENT_NAME, VR::PN, "Some^Name"),
                DataElement::new(tags::STUDY_DATE, VR::DA, "20150328"),
                DataElement::new(tags::PATIENT_AGE, VR::AS, "023W"),
            ]),
            InMemDicomObject::from_element_iter([
                DataElement::new(tags::PATIENT_ID, VR::PN, "09887"),
                DataElement::new(tags::PATIENT_NAME, VR::PN, "Other^Name"),
                DataElement::new(tags::STUDY_DATE, VR::DA, "20250820"),
                DataElement::new(tags::STUDY_TIME, VR::TM, "053010"),
                DataElement::new(tags::DATE_TIME, VR::DT, "20250820053010"),
                DataElement::new(tags::PATIENT_AGE, VR::AS, "056Y"),
            ]),
        ];

        let dict = StandardDataDictionary;
        let tag_type = TagType::Keyword;

        let query_tags = [
            tags::PATIENT_NAME,
            tags::STUDY_DATE,
            tags::STUDY_TIME,
            tags::DATE_TIME,
            tags::PATIENT_AGE,
        ];

        let header_row: Vec<String> = query_tags
            .iter()
            .map(|t| match tag_type {
                TagType::Keyword => dict
                    .by_tag(*t)
                    .map(|e| e.alias.to_string())
                    .unwrap_or_else(|| t.to_string()), // fallback for possibly missing tag not in
                // current version of StandardDataDictionary or is private tag
                TagType::Hex => t.to_string(),
            })
            .collect::<Vec<String>>();

        // let ser = dicom_json::to_string(&ds);
        let tag_values = datasets
            .iter()
            .map(|ds| extract_tag_values(&query_tags, ds))
            .collect::<Vec<Vec<ValueType>>>();
        println!("{header_row:?}");
        println!("{tag_values:?}");

        let json = serde_json::to_string_pretty(&TagData {
            header: header_row,
            values: tag_values,
        });
        println!("{json:?}");
    }
}
