use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::{fmt, str::FromStr};

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::ops::{ApplyOp, AttributeAction, AttributeOp};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{StandardDataDictionary, tags};
use dicom_object::{InMemDicomObject, mem::InMemElement};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, Whatever, whatever};
use tracing::warn;

use crate::error::{CsvSnafu, DeserError, InvalidHeaderTagSnafu, JsonSnafu, OpenFileSnafu};
use crate::query::TermQuery;
use crate::utils::{parse_date_range, parse_datetime, parse_time, push_unique_tags};
use crate::{DatasetsFromFileSnafu, DeserDatasetsFromFileSnafu, Error, FileExtension};

#[derive(Debug)]
pub enum TagScope {
    /// CSV: single tags header shared across all studies
    Csv(Vec<Tag>),
    /// JSON: independent tags per study - tags[i] -> query[i]
    Json(Vec<Vec<Tag>>),
}

#[derive(Debug)]
pub struct DicomQuerySet {
    pub queries: Vec<InMemDicomObject>,
    pub tags: TagScope,
}

impl Default for DicomQuerySet {
    fn default() -> Self {
        let ds = InMemDicomObject::from_element_iter([DataElement::new(
            tags::STUDY_INSTANCE_UID,
            VR::UI,
            PrimitiveValue::Empty,
        )]);

        let tags = TagScope::Json(vec![vec![tags::STUDY_INSTANCE_UID]]);

        // JsonStudyQueries(vec![(ds, HashSet::from([tags::STUDY_INSTANCE_UID]))])
        DicomQuerySet {
            queries: vec![ds],
            tags,
        }
    }
}

impl DicomQuerySet {
    pub fn with_per_query_tags(items: Vec<(InMemDicomObject, Vec<Tag>)>) -> Self {
        let (queries, tags) = items.into_iter().unzip();
        Self {
            queries,
            tags: TagScope::Json(tags),
        }
    }

    pub fn with_shared_tags(queries: Vec<InMemDicomObject>, tags: Vec<Tag>) -> Self {
        Self {
            queries,
            tags: TagScope::Csv(tags),
        }
    }

    pub fn queries_from_file(path: PathBuf) -> Result<Self, Error> {
        let file_ext = path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .parse::<FileExtension>()
            .map_err(|reason| DatasetsFromFileSnafu { reason }.build())?;

        let file = File::open(&path)
            .context(OpenFileSnafu { path })
            .context(DeserDatasetsFromFileSnafu)?;

        match file_ext {
            FileExtension::Csv => queries_from_csv(file),
            FileExtension::Json => queries_from_json(file), // datasets_from_json(file),
        }
        .context(DeserDatasetsFromFileSnafu)
    }

    pub fn add_terminal_tags(&mut self, term_tags: &[TermQuery]) -> Result<(), crate::Error> {
        for t in term_tags {
            let value = term_to_value(t.selector.last_tag(), &t.value)
                .whatever_context("failed overriding tags")?;

            for ds in self.queries.iter_mut() {
                if ds.get(t.selector.last_tag()).is_none() {
                    ds.apply(AttributeOp::new(
                        t.selector.clone(),
                        AttributeAction::Set(value.to_owned()),
                    ))
                    .with_whatever_context(|e| {
                        format!("Could not set terminal tag attribute {}: {e}", t.selector)
                    })?;
                }
            }
        }
        Ok(())
    }

    pub fn merge_tags(&mut self, extra_tags: Vec<TermQuery>) {
        let extra_tags: Vec<Tag> = extra_tags.iter().map(|t| t.selector.last_tag()).collect();
        match &mut self.tags {
            TagScope::Csv(vec_of_tags) => push_unique_tags(vec_of_tags, &extra_tags),
            TagScope::Json(vec_vec_tags) => vec_vec_tags
                .iter_mut()
                .for_each(|vt| push_unique_tags(vt, &extra_tags)),
        }
    }

    pub fn queries(&self) -> &[InMemDicomObject] {
        &self.queries
    }

    pub fn len(&self) -> usize {
        self.queries.len()
    }
    pub fn tags_for(&self, i: usize) -> &[Tag] {
        tags_of(&self.tags, i)
    }

    pub fn all_tags(&self) -> Vec<Tag> {
        let mut tags = match &self.tags {
            TagScope::Csv(vt) => vt.clone(),
            TagScope::Json(vvt) => {
                let mut seen = HashSet::new();
                vvt.iter()
                    .flatten()
                    .copied()
                    .filter(|t| seen.insert(*t))
                    .collect()
            }
        };
        tags.sort();
        tags
    }
}

fn tags_of(scope: &TagScope, i: usize) -> &[Tag] {
    match scope {
        TagScope::Csv(vt) => vt,        // &Vec<Tag>
        TagScope::Json(vvt) => &vvt[i], // &Vec<Vec<Tag>>
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
        f.write_str("a map of DICOM keyword or \"gggg,eeee\" to value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<SingleJsonStudy, A::Error> {
        let dict = StandardDataDictionary;
        let mut elements = Vec::new();
        let mut tags = Vec::new();
        let mut seen: HashSet<Tag> = HashSet::new();

        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            let header_tag = resolve_header_tag(&dict, &key).map_err(de::Error::custom)?;
            if !seen.insert(header_tag.tag) {
                warn!("Duplicate query tag: {key}-{value}");
            } else {
                tags.push(header_tag.tag);
            }
            // TODO: change to term_to_value
            let element = build_element(header_tag.tag, header_tag.vr, &value).map_err(|e| {
                de::Error::custom(format!(
                    "Failed creating element for tag {key}-{value}: {e}"
                ))
            })?;
            elements.push(element);
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
        f.write_str("a DICOM object map or an array of DICOM object maps")
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

fn queries_from_json<R: Read>(reader: R) -> Result<DicomQuerySet, DeserError> {
    Ok(serde_json::from_reader::<_, JsonQueries>(reader)
        .context(JsonSnafu)?
        .0)
}

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

// TODO: replace this function with term_to_value() if possible
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

fn queries_from_csv<R: Read>(reader: R) -> Result<DicomQuerySet, DeserError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b';')
        .trim(csv::Trim::All)
        .from_reader(reader);

    let dict = StandardDataDictionary;
    let mut tags = Vec::new();
    let mut seen = HashSet::new();
    for key in reader.headers().context(CsvSnafu)?.iter() {
        let ht = resolve_header_tag(&dict, key)?;
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
                let prim_value = term_to_value(tag, cell)?;
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

        let JsonQueries(DicomQuerySet { queries, tags }) =
            serde_json::from_str(json_object).expect("Failed deserializing JSON");
        assert_eq!(queries.len(), 1);
        println!("{tags:?}");
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

        let JsonQueries(DicomQuerySet { queries, tags }) =
            serde_json::from_str(json_object).expect("Failed deserializing JSON");
        assert_eq!(queries.len(), 2);
    }

    // #[test]
    // fn deser_json_file() {
    //     let path = "./output/res.json";
    //     let file = std::fs::File::open(path)
    //         .context(OpenFileSnafu { path })
    //         .expect("Failed opening file");
    //     let datasets: Result<JsonStudyQueries, DeserError> =
    //         serde_json::from_reader(std::io::BufReader::new(file)).context(JsonSnafu);
    //     assert!(datasets.is_ok());
    // }

    // #[test]
    // fn deser_json_file_fail() {
    //     let path = "./output/nofile.json";
    //     let err = datasets_from_json(path.into());
    //     assert!(err.is_err());
    // }
}
