use std::{fmt, str::FromStr};

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_object::{InMemDicomObject, mem::InMemElement};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde_json::Value;
use snafu::{ResultExt, Whatever, whatever};

use crate::utils::{parse_date_range, parse_datetime, parse_time};

#[derive(Debug)]
pub struct DicomObjectQueries(InMemDicomObject);

impl<'de> Deserialize<'de> for DicomObjectQueries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(TagSetVisitor)
    }
}

struct TagSetVisitor;

impl<'de> Visitor<'de> for TagSetVisitor {
    type Value = DicomObjectQueries;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of DICOM keyword or \"gggg,eeee\" to value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DicomObjectQueries, A::Error> {
        let mut elements = Vec::new();

        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            let (tag, vr) = resolve_tag(&key).map_err(de::Error::custom)?;
            let element = build_element(tag, vr, &value)
                .map_err(|e| de::Error::custom(format!("tag {key}: {e}")))?;
            elements.push(element);
        }

        Ok(DicomObjectQueries(InMemDicomObject::from_element_iter(
            elements,
        )))
    }
}

fn resolve_tag(key: &str) -> Result<(Tag, VR), String> {
    let dict = StandardDataDictionary;

    if let Some(tag) = parse_hex_tag(key) {
        // Unknown (e.g. private) tags fall back to UN
        let vr = dict.by_tag(tag).map(|e| e.vr().relaxed()).unwrap_or(VR::UN);
        return Ok((tag, vr));
    }

    let entry = dict
        .by_name(key)
        .ok_or_else(|| format!("unknown tag keyword {key:?}"))?;
    Ok((entry.tag_range().inner(), entry.vr().relaxed()))
}

fn parse_hex_tag(key: &str) -> Option<Tag> {
    let hex: String = key
        .trim_matches(|c| c == '(' || c == ')')
        .chars()
        .filter(|c| *c != ',' && !c.is_whitespace())
        .collect();
    if hex.len() != 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let group = u16::from_str_radix(&hex[..4], 16).ok()?;
    let element = u16::from_str_radix(&hex[4..], 16).ok()?;
    Some(Tag(group, element))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deser_json() {
        let reader = std::fs::File::open("./output/res.json").expect("Failed reading JSON file");
        let deserd: Vec<DicomObjectQueries> =
            serde_json::from_reader(std::io::BufReader::new(reader))
                .expect("Failed deserializing JSON");
        println!("{deserd:?}")
    }
}
