use dicom_core::header::HasLength;
use dicom_core::{DataDictionary, DataElement, Tag, VR};
use dicom_object::{StandardDataDictionary, mem::InMemDicomObject};
use serde::{Serialize, Serializer};
use snafu::{ResultExt, Snafu};
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use tracing::{info, warn};

use dicom_core::VR::*;

use crate::client::FindResult;
use crate::deserialize::DicomQuerySet;
use crate::{CreateOutputFileSnafu, FileExtension};
use crate::{Error, SerializeCsvSnafu, SerializeJsonSnafu};

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SingleValue {
    Int(i64),
    Float(f64),
    Text(String),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum FieldValue {
    Empty,
    One(SingleValue),
    Many(Vec<SingleValue>),
}

impl fmt::Display for SingleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SingleValue::Int(v) => write!(f, "{v}"),
            SingleValue::Float(v) => write!(f, "{v}"),
            SingleValue::Text(v) => f.write_str(v),
        }
    }
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldValue::Empty => Ok(()),
            FieldValue::One(v) => write!(f, "{v}"),
            FieldValue::Many(v) => {
                for (i, v) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str("\\")?;
                    }
                    write!(f, "{v}")?;
                }
                Ok(())
            }
        }
    }
}

struct Row<'a> {
    keys: &'a [String],
    values: &'a [FieldValue],
}

impl Serialize for Row<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.keys.len()))?;
        for (key, value) in self.keys.iter().zip(self.values) {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Debug, Snafu)]
enum SerError {
    #[snafu(display("Unsupported VR {:?}", vr))]
    Unsupported { vr: VR },
    #[snafu(display("Cannot convert {:?} value", vr))]
    Convert {
        source: dicom_core::value::ConvertValueError,
        vr: VR,
    },
    Cast {
        source: dicom_core::value::CastValueError,
        vr: VR,
    },
}

pub fn write_responses(
    path: PathBuf,
    output_format: FileExtension,
    query_set: &DicomQuerySet,
    responses: &[FindResult],
) -> Result<(), Error> {
    let writer = File::create(&path).context(CreateOutputFileSnafu {
        path: path.to_owned(),
    })?;

    match output_format {
        FileExtension::Csv => write_to_csv(writer, query_set, responses),
        FileExtension::Json => write_to_json(writer, query_set, responses),
    }?;
    info!("Written responses to `{}`", path.display());
    Ok(())
}

fn write_to_csv<W: std::io::Write>(
    writer: W,
    query_set: &DicomQuerySet,
    responses: &[FindResult],
) -> Result<(), Error> {
    let dict = StandardDataDictionary;

    let tags = query_set.all_tags();
    let header = tags
        .iter()
        .map(|t| tag_label(*t, &dict))
        .collect::<Vec<String>>();
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_writer(writer);
    writer.write_record(header).context(SerializeCsvSnafu)?;
    for obj in responses.iter().flat_map(|r| &r.matches) {
        let values = extract_row(&tags, obj)
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<String>>();
        writer.write_record(values).context(SerializeCsvSnafu)?;
    }
    Ok(())
}

fn write_to_json<W: std::io::Write>(
    writer: W,
    query_set: &DicomQuerySet,
    responses: &[FindResult],
) -> Result<(), Error> {
    use serde::ser::{SerializeSeq, Serializer};
    let dict = StandardDataDictionary;

    let mut ser = serde_json::Serializer::pretty(writer);
    let mut seq = ser.serialize_seq(None).context(SerializeJsonSnafu)?;

    for resp in responses {
        let tags = query_set.tags_for(resp.query_index);
        let keys: Vec<String> = tags.iter().map(|&t| tag_label(t, &dict)).collect();
        for obj in &resp.matches {
            let values = extract_row(tags, obj);
            seq.serialize_element(&Row {
                keys: &keys,
                values: &values,
            })
            .context(SerializeJsonSnafu)?;
        }
    }
    seq.end().context(SerializeJsonSnafu)?;
    Ok(())
}

fn extract_row(tags: &[Tag], obj: &InMemDicomObject) -> Vec<FieldValue> {
    tags.iter()
        .map(|t| match obj.element(*t) {
            Ok(el) => parse_element(el).unwrap_or_else(|e| {
                warn!("Tag {t}: {e}, falling back to raw text");
                el.value().to_str().map_or(FieldValue::Empty, |s| {
                    FieldValue::One(SingleValue::Text(s.into_owned()))
                })
            }),
            Err(_) => FieldValue::Empty,
        })
        .collect()
}

fn parse_element(el: &DataElement<InMemDicomObject>) -> Result<FieldValue, SerError> {
    if el.value().is_empty() {
        return Ok(FieldValue::Empty);
    }

    let vr = el.vr();
    let values: Vec<SingleValue> = match vr {
        AE | AS | CS | LO | LT | PN | SH | ST | UI | UR | UT | UC | DA | TM | DT => el
            .value()
            .to_multi_str()
            .context(CastSnafu { vr })?
            .iter()
            .map(|s| SingleValue::Text(s.clone()))
            .collect(),
        IS | SS | SL | US | UL => el
            .value()
            .to_multi_int::<i64>()
            .context(ConvertSnafu { vr })?
            .into_iter()
            .map(SingleValue::Int)
            .collect(),
        DS | FL | FD => el
            .value()
            .to_multi_float64()
            .context(ConvertSnafu { vr })?
            .into_iter()
            .map(SingleValue::Float)
            .collect(),
        _ => return Err(UnsupportedSnafu { vr }.build()),
    };

    Ok(match values.len() {
        0 => FieldValue::Empty,
        1 => FieldValue::One(values.into_iter().next().unwrap()),
        _ => FieldValue::Many(values),
    })
}

fn tag_label(tag: Tag, dict: &StandardDataDictionary) -> String {
    dict.by_tag(tag).unwrap().alias.to_string()
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
