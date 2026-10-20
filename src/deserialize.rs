use std::collections::HashSet;
use std::fmt;
use std::io::Read;

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_object::InMemDicomObject;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, Whatever, whatever};
use tracing::warn;

use crate::error::{CsvSnafu, DeserError, InvalidHeaderTagSnafu, JsonSnafu};
use crate::query::DicomQuerySet;
use crate::utils::{parse_date_range, parse_datetime, parse_time};

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct HeaderTag {
    pub tag: Tag,
    pub vr: VR,
}

impl HeaderTag {
    fn from_standard_dict(header: &str, dict: &StandardDataDictionary) -> Result<Self, DeserError> {
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
}

struct SingleJsonStudy(InMemDicomObject, Vec<Tag>);
struct StudyVisitor;

impl<'de> Deserialize<'de> for SingleJsonStudy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StudyVisitor)
    }
}

impl<'de> Visitor<'de> for StudyVisitor {
    type Value = SingleJsonStudy;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map `{ ... }` of DICOM keyword or \"gggg,eeee\" to value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<SingleJsonStudy, A::Error> {
        let dict = StandardDataDictionary;
        let mut elements = Vec::new();
        let mut tags = Vec::new();
        let mut seen: HashSet<Tag> = HashSet::new();

        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            let header_tag =
                HeaderTag::from_standard_dict(&key, &dict).map_err(de::Error::custom)?;
            if !seen.insert(header_tag.tag) {
                warn!("Duplicate query tag: {key}-{value}");
            } else {
                tags.push(header_tag.tag);
            }
            let str_value = json_value_to_string(&value).map_err(|e| {
                de::Error::custom(format!(
                    "Failed creating element for tag {key}-{value}: {e}"
                ))
            })?;

            let prim_value =
                term_to_value(header_tag.tag, &str_value).map_err(de::Error::custom)?;
            elements.push(DataElement::new(header_tag.tag, header_tag.vr, prim_value));
        }

        Ok(SingleJsonStudy(
            InMemDicomObject::from_element_iter(elements),
            tags,
        ))
    }
}

/// JSON: header tags per study
/// expecting `{ ... }`, `[ { ... } ]` or `[ { ... }, ... ]`
struct JsonQueries(DicomQuerySet);
struct QueriesVisitor;

impl<'de> Deserialize<'de> for JsonQueries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(QueriesVisitor)
    }
}

impl<'de> Visitor<'de> for QueriesVisitor {
    type Value = JsonQueries;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(
            "a DICOM object map `{ ... }` or an array of DICOM object maps `[ { ... }, ... ]`",
        )
    }

    // deserialize sequence of maps: [ { ... }, { ... }, ... ] or [ { ... } ]
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JsonQueries, A::Error> {
        let mut objects = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(SingleJsonStudy(ds, tags)) = seq.next_element()? {
            objects.push((ds, tags));
        }
        if objects.is_empty() {
            return Err(de::Error::custom("No queries found in array"));
        }
        Ok(JsonQueries(DicomQuerySet::with_per_query_tags(objects)))
    }

    // deserialize single map { ... }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<JsonQueries, A::Error> {
        let SingleJsonStudy(ds, tags) =
            SingleJsonStudy::deserialize(de::value::MapAccessDeserializer::new(map))?;

        Ok(JsonQueries(DicomQuerySet::with_per_query_tags(vec![(
            ds, tags,
        )])))
    }
}

pub fn queries_from_json<R: Read>(reader: R) -> Result<DicomQuerySet, DeserError> {
    Ok(serde_json::from_reader::<_, JsonQueries>(reader)
        .context(JsonSnafu)?
        .0)
}

fn json_value_to_string(value: &Value) -> Result<String, Whatever> {
    let text = match value {
        Value::Null => String::new(),
        Value::String(s) if matches!(s.to_lowercase().as_str(), "nan" | "null") => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => whatever!("unsupported JSON value {other}"),
    };
    Ok(text)
}

pub fn term_to_value(tag: Tag, str_value: &str) -> Result<PrimitiveValue, Whatever> {
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
            let value =
                parse_date_range(str_value).whatever_context("Failed parsing date range")?;
            PrimitiveValue::from(value)
        }
        VR::TM => {
            let value = parse_time(str_value).whatever_context("failed parsing time")?;
            PrimitiveValue::from(value)
        }
        VR::DT => {
            let value = parse_datetime(str_value).whatever_context("Failed parsing datetime")?;
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

pub fn queries_from_csv<R: Read>(reader: R) -> Result<DicomQuerySet, DeserError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b';')
        .trim(csv::Trim::All)
        .from_reader(reader);

    let dict = StandardDataDictionary;
    let mut tags = Vec::new();
    let mut seen = HashSet::new();
    for key in reader.headers().context(CsvSnafu)?.iter() {
        let ht = HeaderTag::from_standard_dict(key, &dict)?;
        if !seen.insert(ht.tag) {
            warn!("Duplicate query tag: {key}");
        } else {
            tags.push(ht);
        }
    }

    let mut queries = Vec::new();
    for record in reader.records() {
        let record = record.context(CsvSnafu)?;
        let mut elements = Vec::with_capacity(tags.len());
        for (&HeaderTag { tag, vr }, cell) in tags.iter().zip(record.iter()) {
            let el = if cell.is_empty() | matches!(cell.to_lowercase().as_str(), "nan" | "null") {
                DataElement::empty(tag, vr)
            } else {
                let prim_value = term_to_value(tag, cell).whatever_context("")?;
                DataElement::new(tag, vr, prim_value)
            };
            elements.push(el);
        }
        queries.push(InMemDicomObject::from_element_iter(elements));
    }

    if queries.is_empty() {
        whatever!("CSV input file has no data rows")
    }

    let tags = tags.iter().map(|ht| ht.tag).collect::<Vec<Tag>>();
    Ok(DicomQuerySet::with_shared_tags(queries, tags))
}

#[cfg(test)]
mod tests {
    use dicom_core::header::HasLength;
    use dicom_dictionary_std::tags;

    use super::*;

    #[test]
    fn test_json_deser_one() {
        let json_object = r#"
        {
            "StudyDate": "2000-01-01",
            "PatientID": "ID01",
            "StudyInstanceUID": "1.2.3",
            "StudyDescription": "",
            "ModalitiesInStudy": "nAn"
        }"#;

        let queries = queries_from_json(json_object.as_bytes()).expect("Failed deserializing JSON");
        assert_eq!(queries.len(), 1);

        let dcm_object = queries.queries.first().unwrap();
        let expected_vals = &[
            (tags::PATIENT_ID, Some("ID01")),
            (tags::STUDY_DESCRIPTION, None),
            (tags::MODALITIES_IN_STUDY, None),
        ];

        for (tag, expected) in expected_vals {
            let value = dcm_object
                .value_at(*tag)
                .unwrap_or_else(|e| panic!("Expected a DataElement for {tag}: {e}"));
            match expected {
                Some(s) => assert_eq!(value.to_str().unwrap(), *s, "{tag}"),
                None => assert!(value.is_empty(), "{tag} should be empty, got {value:?}"),
            }
        }
    }

    #[test]
    fn test_json_deser_many() {
        // JSON parsed DICOM objects allows variable number of tags
        let json_object = r#"
        [
            {
                "StudyDate": "1999-01-02",
                "PatientID": "ID01",
                "StudyInstanceUID": "1.2.3"
            },
            {
                "StudyDate": "2000-01-01",
                "PatientID": "ID02",
                "StudyInstanceUID": "2.5.6",
                "StudyDescription": "",
                "ModalitiesInStudy": "nAn"
            }
        ]"#;

        let queries = queries_from_json(json_object.as_bytes()).expect("Failed deserializing JSON");
        assert_eq!(
            queries.len(),
            2,
            "Expected 2 objects, got {}",
            queries.len()
        );

        for (obj, expected_count) in queries.queries.iter().zip([3, 5]) {
            let num_of_tags = obj.tags().count();
            assert_eq!(
                num_of_tags, expected_count,
                "Expected {expected_count} tags, got {num_of_tags}"
            );
        }
    }

    #[test]
    fn test_csv_deser_many() {
        // All DICOM objects always share the same header tags
        let csv_object = r"
        PatientID;StudyInstanceUID;StudyDescription;ModalitiesInStudy
        ID01;1.2.3;;
        ID02;2.5.6;nAn;
        ";
        let queries =
            queries_from_csv(csv_object.trim().as_bytes()).expect("Failed deserializing CSV");

        for obj in queries.queries {
            let num_of_tags = obj.tags().count();
            assert_eq!(num_of_tags, 4, "Expected 4 tags, got {num_of_tags}");
        }
    }

    #[test]
    fn test_queries_from_json() {
        let reader =
            std::fs::File::open("example/example-many.json").expect("Expected a valid JSON file");
        let queries = queries_from_json(reader).expect("Failed deserializing JSON file");
        assert!(
            queries.len() >= 1,
            "Expected at least 1 query object, got none"
        );
    }

    #[test]
    fn test_queries_from_csv() {
        let reader = std::fs::File::open("example/example.csv").expect("Expected a valid CSV file");
        let queries = queries_from_csv(reader).expect("Failed deserializing CSV file");
        assert!(
            queries.len() >= 1,
            "Expected at least 1 query object, got none"
        );
    }
}
