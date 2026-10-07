use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::{fmt, str::FromStr};

use crate::error::{InvalidHeaderTagSnafu, JsonSnafu, OpenFileSnafu};
use crate::utils::{parse_date_range, parse_datetime, parse_time};
use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{StandardDataDictionary, tags};
use dicom_object::{InMemDicomObject, mem::InMemElement};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, Whatever, whatever};

use crate::error::{CsvSnafu, DeserError};

#[derive(Debug)]
pub struct StudyJson(InMemDicomObject, HeaderTags);
struct StudyVisitor;

type HeaderTags = HashSet<HeaderTag>;

impl<'de> Deserialize<'de> for StudyJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StudyVisitor)
    }
}

impl<'de> Visitor<'de> for StudyVisitor {
    type Value = StudyJson;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of DICOM keyword or \"gggg,eeee\" to value")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StudyJson, A::Error> {
        let dict = StandardDataDictionary;
        let mut elements = Vec::new();
        let mut header_tags: HeaderTags = HashSet::new();

        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            let header_tag = resolve_header_tag(&dict, &key).map_err(de::Error::custom)?;
            let element = build_element(header_tag.tag, header_tag.vr, &value)
                .map_err(|e| de::Error::custom(format!("tag {key}: {e}")))?;
            elements.push(element);
            header_tags.insert(header_tag);
        }

        Ok(StudyJson(
            InMemDicomObject::from_element_iter(elements),
            header_tags,
        ))
    }
}

impl<'de> Deserialize<'de> for DicomObjectQueries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(QueriesVisitor)
    }
}

#[derive(Debug)]
pub struct DicomObjectQueries(pub Vec<StudyJson>);

struct QueriesVisitor;

impl<'de> Visitor<'de> for QueriesVisitor {
    type Value = DicomObjectQueries;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of DICOM keyword or \"gggg,eeee\" to value")
    }

    // deserialize sequence of maps: [ { ... }, { ... } ]
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<DicomObjectQueries, A::Error> {
        let mut objects = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(study) = seq.next_element()? {
            objects.push(study);
        }
        Ok(DicomObjectQueries(objects))
    }

    // deserialize single map { ... }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<DicomObjectQueries, A::Error> {
        let study = StudyJson::deserialize(de::value::MapAccessDeserializer::new(map))?;
        Ok(DicomObjectQueries(vec![study]))
    }
}

// TODO: finish this function and add it to build_queries()
// TODO json deserialization must return Vec<HeaderTag>
// pub fn datasets_from_json(
//     path: PathBuf,
// ) -> Result<(DicomObjectQueries, Vec<HeaderTag>), DeserError> {
//     let file = std::fs::File::open(path).context(OpenFileSnafu { path })?;
//     let datasets: DicomObjectQueries =
//         serde_json::from_reader(std::io::BufReader::new(file)).context(JsonSnafu { path });
//
//     Ok((DicomObjectQueries,))
// }

// fn resolve_tag(key: &str) -> Result<(Tag, VR), String> {
//     let dict = StandardDataDictionary;
//
//     if let Some(tag) = parse_hex_tag(key) {
//         // Unknown (e.g. private) tags fall back to UN
//         let vr = dict.by_tag(tag).map(|e| e.vr().relaxed()).unwrap_or(VR::UN);
//         return Ok((tag, vr));
//     }
//
//     let entry = dict
//         .by_name(key)
//         .ok_or_else(|| format!("unknown tag keyword {key:?}"))?;
//     Ok((entry.tag_range().inner(), entry.vr().relaxed()))
// }

// fn parse_hex_tag(key: &str) -> Option<Tag> {
//     let hex: String = key
//         .trim_matches(|c| c == '(' || c == ')')
//         .chars()
//         .filter(|c| *c != ',' && !c.is_whitespace())
//         .collect();
//     if hex.len() != 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
//         return None;
//     }
//     let group = u16::from_str_radix(&hex[..4], 16).ok()?;
//     let element = u16::from_str_radix(&hex[4..], 16).ok()?;
//     Some(Tag(group, element))
// }

fn build_element(tag: Tag, vr: VR, value: &Value) -> Result<InMemElement, Whatever> {
    let text = match value {
        Value::Null => return Ok(DataElement::empty(tag, vr)),
        Value::String(s) if matches!(s.to_lowercase().as_str(), "nan" | "null") => {
            return Ok(DataElement::empty(tag, vr));
        }
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => whatever!("unsupported JSON value {other}"),
    };
    // TODO: try to replace with term_to_value()
    Ok(DataElement::new(tag, vr, to_primitive(vr, &text)?))
}

fn to_primitive(vr: VR, s: &str) -> Result<PrimitiveValue, Whatever> {
    match vr {
        VR::US => parse_number::<u16>(s),
        VR::UL => parse_number::<u32>(s),
        VR::SS => parse_number::<i16>(s),
        VR::SL => parse_number::<i32>(s),
        VR::FL => parse_number::<f32>(s),
        VR::FD => parse_number::<f64>(s),
        VR::OB | VR::OW | VR::OF | VR::OD | VR::OL | VR::SQ | VR::UN => {
            whatever!("VR {vr:?} is not supported from JSON text")
        }
        // All remaining VRs are text-based (DA, PN, UI, LO, IS, DS, ...)
        VR::DA => {
            let value = parse_date_range(s)?;
            Ok(PrimitiveValue::from(value))
        }
        VR::TM => {
            let value = parse_time(s)?;
            Ok(PrimitiveValue::from(value))
        }
        VR::DT => {
            let value = parse_datetime(s)?;
            Ok(PrimitiveValue::from(value))
        }
        _ => {
            let parts: Vec<String> = s.split('\\').map(str::to_owned).collect();
            Ok(if parts.len() > 1 {
                PrimitiveValue::Strs(parts.into())
            } else {
                PrimitiveValue::Str(s.to_owned())
            })
        }
    }
}

pub fn term_to_value(tag: Tag, str_value: &str) -> Result<PrimitiveValue, DeserError> {
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
            let value = parse_date_range(str_value).whatever_context("fff")?;
            PrimitiveValue::from(value)
        }
        VR::TM => {
            let value = parse_time(str_value).whatever_context("ff")?;
            PrimitiveValue::from(value)
        }
        VR::DT => {
            let value = parse_datetime(str_value).whatever_context("fff")?;
            PrimitiveValue::from(value)
        }

        VR::AT | VR::OB | VR::OD | VR::OF | VR::OL | VR::OV | VR::OW | VR::UN => {
            whatever!("Unsupported VR {vr}")
        }
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

fn parse_number<T>(s: &str) -> Result<PrimitiveValue, Whatever>
where
    T: FromStr,
    T::Err: std::error::Error + 'static + std::marker::Sync + std::marker::Send,
    PrimitiveValue: From<T>,
{
    s.trim()
        .parse::<T>()
        .with_whatever_context(|e| format!("{s:?}: {e}"))
        .map(PrimitiveValue::from)
}

pub fn datasets_from_csv(
    file: PathBuf,
) -> Result<(DicomObjectQueries, Vec<HeaderTag>), DeserError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b';')
        .flexible(true)
        .from_path(&file)
        .context(CsvSnafu)?;
    let dict = StandardDataDictionary;
    let headers = reader.headers().context(CsvSnafu)?;
    let tags = headers
        .iter()
        .map(|h| resolve_header_tag(&dict, h))
        .collect::<Result<HeaderTags, DeserError>>()?;
    let datasets: Vec<InMemDicomObject> = reader
        .records()
        .map(|row| {
            let row = row.context(CsvSnafu)?;
            row_to_dataset(&tags, &row)
        })
        .collect::<Result<Vec<InMemDicomObject>, DeserError>>()?;
    // .whatever_context("Could not create datasets from file")?;
    Ok(DicomObjectQueries())
}

fn row_to_dataset(
    tags: &HeaderTags,
    row: &csv::StringRecord,
) -> Result<InMemDicomObject, DeserError> {
    let elements = tags
        .iter()
        .zip(row.iter())
        .map(|(header_tag, str_value)| {
            let value = term_to_value(header_tag.tag, str_value).with_whatever_context(|_| {
                format!("Bad value {str_value:?} for tag {}", header_tag.tag)
            })?;
            Ok(DataElement::new(header_tag.tag, header_tag.vr, value))
        })
        .collect::<Result<Vec<InMemElement>, DeserError>>()?;
    Ok(InMemDicomObject::from_element_iter(elements))
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct HeaderTag {
    pub tag: Tag,
    pub vr: VR,
}

fn resolve_header_tag(
    dict: &StandardDataDictionary,
    header: &str,
) -> Result<HeaderTag, DeserError> {
    let entry = dict
        .by_expr(header)
        .context(InvalidHeaderTagSnafu { header })?;
    let vr = entry
        .vr()
        .exact()
        .whatever_context(format!("Unsupported VR for tag: {}", entry.tag()))?;

    Ok(HeaderTag {
        tag: entry.tag(),
        vr,
    })
}

pub fn default_dataset() -> (DicomObjectQueries, Vec<HeaderTag>) {
    let study_uid_tag = HeaderTag {
        tag: tags::STUDY_INSTANCE_UID,
        vr: VR::UI,
    };
    let ds = InMemDicomObject::from_element_iter([DataElement::new(
        study_uid_tag.tag,
        study_uid_tag.vr,
        PrimitiveValue::Empty,
    )]);

    (DicomObjectQueries(vec![ds]), vec![study_uid_tag])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deser_single_object() {
        let json_object = r#"
        {
            "StudyDate": "2000-01-01",
            "PatientID": "LIDC-IDRI-0580",
            "StudyInstanceUID": "1.3.6.1.4.1.14519.5.2.1.6279.6001.173480979711457247360986415860",
            "StudyDescription": "",
            "ModalitiesInStudy": "nAn"
        }"#;

        let DicomObjectQueries(objects) =
            serde_json::from_str(json_object).expect("Failed deserializing JSON");
        assert_eq!(objects.len(), 1);
    }

    #[test]
    fn deser_multiple_object() {
        let json_object = r#"
        [
            {
                "StudyDate": "1999-01-02",
                "PatientID": "112516",
                "StudyInstanceUID": "1.2.840.113654.2.55.33575893932308185246496913106863435791"
            },
            {
                "StudyDate": "2000-01-01",
                "PatientID": "LIDC-IDRI-0580",
                "StudyInstanceUID": "1.3.6.1.4.1.14519.5.2.1.6279.6001.173480979711457247360986415860",
                "StudyDescription": "",
                "ModalitiesInStudy": "nAn"
            }
        ]"#;

        let DicomObjectQueries(objects) =
            serde_json::from_str(json_object).expect("Failed deserializing JSON");
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn deser_json_file() {
        let path = "./output/res.json";
        let file = std::fs::File::open(path)
            .context(OpenFileSnafu { path })
            .expect("Failed opening file");
        let datasets: DicomObjectQueries = serde_json::from_reader(std::io::BufReader::new(file))
            .context(JsonSnafu { path })
            .expect("Failed deserializing JSON");
        println!("{datasets:?}")
    }
}
