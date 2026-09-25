use dicom_core::{DataDictionary, DataElement, Tag};
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snafu::ResultExt;
use std::fs::File;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use dicom_core::VR::*;

use crate::FileExtension;
use crate::{CreateOutputFileSnafu, Error, SerializeCsvSnafu, SerializeJsonSnafu};

pub fn serialize_responses(
    path: PathBuf,
    response_datasets: Vec<InMemDicomObject>,
    tag_queries: Vec<Tag>,
    file_extension: FileExtension,
) -> Result<(), Error> {
    let dict = StandardDataDictionary;
    let header: Vec<String> = tag_queries
        .iter()
        .map(|t| {
            dict.by_tag(*t)
                .map(|e| e.alias.to_string())
                .unwrap_or_else(|| {
                    // fallback for tag possibly missing in current version of StandardDataDictionary or is private tag
                    warn!("Tag {t} not found in StandardDataDictionary or is PrivateTag");
                    t.to_string()
                })
        })
        .collect::<Vec<String>>();

    let values = response_datasets
        .iter()
        .map(|ds| extract_tag_values(&tag_queries, ds))
        .collect::<Vec<Vec<ValueType>>>();

    let data = TagData { header, values };

    match file_extension {
        FileExtension::Csv => write_to_csv(&path, data),
        FileExtension::Json => write_to_json(&path, data),
    }?;

    info!(
        "Wrote {} responses to {}",
        response_datasets.len(),
        path.display()
    );
    Ok(())
}

fn write_to_json(path: &Path, data: TagData) -> Result<(), Error> {
    let writer = File::create(path).context(CreateOutputFileSnafu { path })?;
    // reformat to json specific data structure to achieve:
    // [
    //  [{"key1", "val", "key2", "val", ...}],
    //  [...],
    // ]
    let TagData { header, values } = data;
    let data_reformatted: Vec<Map<String, Value>> = values
        .into_iter()
        .map(|row| {
            header
                .iter()
                .cloned()
                .zip(row)
                .map(|(key, val)| {
                    let val = serde_json::to_value(val)
                        .expect("ValueType is a simple enum and always serializes");
                    (key, val)
                })
                .collect()
        })
        .collect();
    serde_json::to_writer_pretty(writer, &data_reformatted).context(SerializeJsonSnafu)
}

fn write_to_csv(path: &Path, data: TagData) -> Result<(), Error> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_path(path)
        .map_err(std::io::Error::from)
        .context(CreateOutputFileSnafu { path })?;

    let TagData { header, values } = data;
    writer.write_record(&header).context(SerializeCsvSnafu)?;
    for row in values {
        let formatted: Vec<String> = row.iter().map(ToString::to_string).collect();
        writer.write_record(&formatted).context(SerializeCsvSnafu)?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ValueType {
    SignedInteger(i32),
    UnsignedInteger(u32),
    Float(f64),
    Text(String),
    AgeString(String),
    Error(DicomValueParseError),
}

impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValueType::SignedInteger(v) => write!(f, "{v}"),
            ValueType::UnsignedInteger(v) => write!(f, "{v}"),
            ValueType::Float(v) => write!(f, "{v}"),
            ValueType::Text(s) | ValueType::AgeString(s) => write!(f, "{s}"),
            ValueType::Error(e) => write!(f, "{e:?}"), // or a Display impl on DicomValueParseError
        }
    }
}

pub(crate) struct TagData {
    header: Vec<String>,
    values: Vec<Vec<ValueType>>,
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

        let query_tags = [
            tags::PATIENT_NAME,
            tags::STUDY_DATE,
            tags::STUDY_TIME,
            tags::DATE_TIME,
            tags::PATIENT_AGE,
        ];

        // let ser = dicom_json::to_string(&ds);

        // let json = serde_json::to_string_pretty(&TagData {
        //     header
        //     values,
        // });
        // println!("{json:?}");
    }
}
