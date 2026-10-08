use dicom_core::DataDictionary;
use dicom_core::ops::AttributeSelector;
use dicom_dictionary_std::StandardDataDictionary;
use std::path::PathBuf;

use snafu::{OptionExt, ResultExt, Whatever};
use std::str::FromStr;

use crate::Error;
use crate::deserialize::DicomQuerySet;

pub fn build_queries(
    file: Option<PathBuf>,
    term_tags: Vec<TermQuery>,
    // information_model: &InformationLevel,
    _verbose: bool,
) -> Result<DicomQuerySet, Error> {
    let mut query_datasets = match file {
        Some(file) => DicomQuerySet::queries_from_file(file)?, //datasets_from_file(file)?,
        None => DicomQuerySet::default(),
    };

    query_datasets.add_terminal_tags(&term_tags)?;

    query_datasets.merge_tags(term_tags);
    Ok(query_datasets)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deserialize::term_to_value;
    use dicom_core::PrimitiveValue;
    use dicom_dictionary_std::tags;

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
