use dicom_core::ops::{ApplyOp, AttributeAction, AttributeOp, AttributeSelector};
use dicom_core::{
    DataDictionary, DataElement, PrimitiveValue, Tag, VR, dictionary::DataDictionaryEntry,
};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_dictionary_std::tags;
use dicom_object::{InMemDicomObject, mem::InMemElement};
use std::path::PathBuf;

use snafu::{OptionExt, ResultExt, Whatever, whatever};
use std::str::FromStr;

use crate::InformationLevel;
use crate::{Error, FileNotFoundSnafu};

use crate::utils::*;

pub fn build_queries(
    file: Option<PathBuf>,
    term_tags: Vec<TermQuery>,
    information_model: &InformationLevel,
    _verbose: bool,
) -> Result<(Vec<InMemDicomObject>, Vec<Tag>), Error> {
    let (datasets, file_tags) = datasets_from_file(file)?;
    let mut datasets = override_tags(datasets, &term_tags)
        .whatever_context("Could not add tags from arguments to datasets")?;

    let level = match information_model {
        InformationLevel::Patient => "PATIENT",
        InformationLevel::Study => "STUDY",
        InformationLevel::Series => "SERIES",
    };

    datasets.iter_mut().for_each(|ds| {
        if ds.get(tags::QUERY_RETRIEVE_LEVEL).is_none() {
            ds.put(DataElement::new(
                tags::QUERY_RETRIEVE_LEVEL,
                VR::CS,
                PrimitiveValue::from(level),
            ));
        }
    });

    let merged_tags = merge_tags(file_tags, term_tags);
    Ok((datasets, merged_tags))
}

fn datasets_from_file(
    file: Option<PathBuf>,
) -> Result<(Vec<InMemDicomObject>, Vec<HeaderTag>), Error> {
    // return default Vec<InMemDicomObject> if no path given
    let Some(file) = file else {
        let study_uid_tag = HeaderTag {
            tag: tags::STUDY_INSTANCE_UID,
            vr: VR::UI,
        };
        return Ok((
            vec![InMemDicomObject::from_element_iter([DataElement::new(
                study_uid_tag.tag,
                study_uid_tag.vr,
                PrimitiveValue::Empty,
            )])],
            vec![study_uid_tag],
        ));
    };

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b';')
        .flexible(true)
        .from_path(&file)
        .context(FileNotFoundSnafu { file })?;

    let dict = StandardDataDictionary;
    let headers = reader
        .headers()
        .whatever_context("Could not read header row from file")?;
    let tags = headers
        .iter()
        .map(|h| resolve_header_tag(&dict, h))
        .collect::<Result<Vec<HeaderTag>, Whatever>>()
        .whatever_context("Could not parse headers to tags")?;

    let datasets: Vec<InMemDicomObject> = reader
        .records()
        .map(|row| {
            let row = row.whatever_context("Could not read row from file")?;
            row_to_dataset(&tags, &row)
        })
        .collect::<Result<Vec<InMemDicomObject>, Whatever>>()
        .whatever_context("Could not create datasets from file")?;
    Ok((datasets, tags))
}

fn row_to_dataset(
    tags: &[HeaderTag],
    row: &csv::StringRecord,
) -> Result<InMemDicomObject, Whatever> {
    let elements = tags
        .iter()
        .zip(row.iter())
        .map(|(header_tag, str_value)| {
            let value = term_to_value(header_tag.tag, str_value).with_whatever_context(|_| {
                format!("Bad value {str_value:?} for tag {}", header_tag.tag)
            })?;
            Ok(DataElement::new(header_tag.tag, header_tag.vr, value))
        })
        .collect::<Result<Vec<InMemElement>, Whatever>>()?;
    Ok(InMemDicomObject::from_element_iter(elements))
}

#[derive(Debug, PartialEq)]
struct HeaderTag {
    tag: Tag,
    vr: VR,
}

fn resolve_header_tag(dict: &StandardDataDictionary, header: &str) -> Result<HeaderTag, Whatever> {
    let entry = dict
        .by_expr(header)
        .whatever_context(format!("Unknown DICOM tag in CSV header row: {header}"))?;
    let vr = entry
        .vr()
        .exact()
        .whatever_context(format!("Unknown VR for tag {}", entry.tag()))?;

    Ok(HeaderTag {
        tag: entry.tag(),
        vr,
    })
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TermQuery {
    pub selector: AttributeSelector,
    pub value: String,
}

impl FromStr for TermQuery {
    type Err = Whatever;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('=');
        let selector_part = parts.next().whatever_context("Empty query")?;
        let value_part = parts.next().unwrap_or_default();
        let selector: AttributeSelector = StandardDataDictionary
            .parse_selector(selector_part)
            .whatever_context(format!(
                "Could not resolve query field path: {selector_part}"
            ))?;

        Ok(TermQuery {
            selector,
            value: value_part.to_owned(),
        })
    }
}

fn override_tags(
    mut datasets: Vec<InMemDicomObject>,
    term_tags: &[TermQuery],
) -> Result<Vec<InMemDicomObject>, Whatever> {
    for t in term_tags {
        let value = term_to_value(t.selector.last_tag(), &t.value)?;

        for ds in datasets.iter_mut() {
            // override a value only if tag is missing
            // TODO: also check for PrimitiveValue::Empty?
            if ds.get(t.selector.last_tag()).is_none() {
                ds.apply(AttributeOp::new(
                    t.selector.clone(),
                    AttributeAction::Set(value.to_owned()),
                ))
                .with_whatever_context(|_| {
                    format!("Could not set query attribute {}", t.selector)
                })?;
            }
        }
    }

    Ok(datasets)
}

fn term_to_value(tag: Tag, str_value: &str) -> Result<PrimitiveValue, Whatever> {
    // silent passthrough -> PACS will return value for this tag instead of matching
    if str_value.is_empty() {
        return Ok(PrimitiveValue::Empty);
    }

    let vr = {
        StandardDataDictionary
            .by_tag(tag)
            .and_then(|e| e.vr.exact())
            .unwrap_or(VR::LO)
    };

    // TODO: implement VR::DT - datetime handling
    let value = match vr {
        VR::AE
        | VR::AS
        | VR::CS
        | VR::DS
        | VR::IS
        | VR::LO
        | VR::LT
        | VR::SH
        | VR::PN
        | VR::ST
        | VR::UI
        | VR::UC
        | VR::UR
        | VR::UT => PrimitiveValue::from(str_value),
        VR::DA => {
            let value = parse_date_range(str_value)?;
            PrimitiveValue::from(value)
        }
        VR::TM => {
            let value = parse_time(str_value)?;
            PrimitiveValue::from(value)
        }
        VR::DT => {
            let value = parse_datetime(str_value)?;
            PrimitiveValue::from(value)
        }
        VR::AT => whatever!("Unsupported VR AT"),
        VR::OB => whatever!("Unsupported VR OB"),
        VR::OD => whatever!("Unsupported VR OD"),
        VR::OF => whatever!("Unsupported VR OF"),
        VR::OL => whatever!("Unsupported VR OL"),
        VR::OV => whatever!("Unsupported VR OV"),
        VR::OW => whatever!("Unsupported VR OW"),
        VR::UN => whatever!("Unsupported VR UN"),
        VR::SQ => whatever!("Unsupported sequence-based query"),
        VR::SS => {
            let ss: i16 = str_value
                .parse()
                .whatever_context("Failed to parse value as SS")?;
            PrimitiveValue::from(ss)
        }
        VR::SL => {
            let sl: i32 = str_value
                .parse()
                .whatever_context("Failed to parse value as SL")?;
            PrimitiveValue::from(sl)
        }
        VR::SV => {
            let sv: i64 = str_value
                .parse()
                .whatever_context("Failed to parse value as SV")?;
            PrimitiveValue::from(sv)
        }
        VR::US => {
            let us: u16 = str_value
                .parse()
                .whatever_context("Failed to parse value as US")?;
            PrimitiveValue::from(us)
        }
        VR::UL => {
            let ul: u32 = str_value
                .parse()
                .whatever_context("Failed to parse value as UL")?;
            PrimitiveValue::from(ul)
        }
        VR::UV => {
            let uv: u64 = str_value
                .parse()
                .whatever_context("Failed to parse value as UV")?;
            PrimitiveValue::from(uv)
        }
        VR::FL => {
            let fl: f32 = str_value
                .parse()
                .whatever_context("Failed to parse value as FL")?;
            PrimitiveValue::from(fl)
        }
        VR::FD => {
            let fd: f64 = str_value
                .parse()
                .whatever_context("Failed to parse value as FD")?;
            PrimitiveValue::from(fd)
        }
    };
    Ok(value)
}

fn merge_tags(file_tags: Vec<HeaderTag>, term_tags: Vec<TermQuery>) -> Vec<Tag> {
    let mut tags = file_tags.iter().map(|t| t.tag).collect::<Vec<Tag>>();
    for t in term_tags.iter() {
        if !tags.contains(&t.selector.last_tag()) {
            tags.push(t.selector.last_tag());
        }
    }
    tags.sort();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime() {
        let datetime_str = "2020-01-02 05:30:20.123";
        let parsed = term_to_value(tags::DATE_TIME, datetime_str).expect("Failed parsing");
        assert_eq!(parsed, PrimitiveValue::from("20200102053020.123000"))
    }

    #[test]
    fn parse_date_range() {
        let date_range_str = "2020-01-02..2022-12-30";
        let parsed = term_to_value(tags::DATE, date_range_str).expect("Failed parsing");
        assert_eq!(parsed, PrimitiveValue::from("20200102-20221230"))
    }

    #[test]
    fn tags_merged_sorted() {}
}
