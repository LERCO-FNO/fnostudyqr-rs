use dicom_core::header::HasLength;
use dicom_core::ops::{ApplyOp, AttributeAction, AttributeOp, AttributeSelector};
use dicom_core::{DataDictionary, DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_dictionary_std::tags::{PATIENT_ID, STUDY_DATE, STUDY_INSTANCE_UID};
use dicom_object::InMemDicomObject;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::path::PathBuf;

use snafu::{OptionExt, ResultExt, Whatever, ensure};
use std::str::FromStr;

use crate::deserialize::{queries_from_csv, queries_from_json, term_to_value};
use crate::utils::{is_output_tag, push_unique_tags};
use crate::{
    DeserDatasetsFromFileSnafu, Error, IOFileFormat, IOFileFormatExtensionSnafu,
    NoRequiredIdentifierSnafu, OpenFileSnafu,
};

#[derive(Debug)]
pub enum TagScope {
    /// CSV: single tags header shared across all studies
    Shared(Vec<Tag>),
    /// JSON: independent tags per study - tags[i] -> queries[i]
    PerQuery(Vec<Vec<Tag>>),
}

#[derive(Debug)]
pub struct DicomQuerySet {
    pub queries: Vec<InMemDicomObject>,
    pub tags: TagScope,
}

impl Default for DicomQuerySet {
    fn default() -> Self {
        // let ds = InMemDicomObject::from_element_iter([DataElement::new(
        //     STUDY_INSTANCE_UID,
        //     VR::UI,
        //     PrimitiveValue::Empty,
        // )]);
        let ds = InMemDicomObject::new_empty();

        // let tags = TagScope::PerQuery(vec![vec![STUDY_INSTANCE_UID]]);
        let tags = TagScope::PerQuery(vec![vec![]]);

        DicomQuerySet {
            queries: vec![ds],
            tags,
        }
    }
}

impl DicomQuerySet {
    pub fn with_per_query_tags(items: Vec<(InMemDicomObject, Vec<Tag>)>) -> Self {
        let (queries, tags) = items
            .into_iter()
            .map(|(obj, mut tags)| {
                tags.retain(is_output_tag);
                (obj, tags)
            })
            .unzip();
        Self {
            queries,
            tags: TagScope::PerQuery(tags),
        }
    }

    pub fn with_shared_tags(queries: Vec<InMemDicomObject>, mut tags: Vec<Tag>) -> Self {
        tags.retain(is_output_tag);
        Self {
            queries,
            tags: TagScope::Shared(tags),
        }
    }

    pub fn queries_from_file(path: PathBuf) -> Result<Self, Error> {
        let file_ext = path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .parse::<IOFileFormat>()
            .map_err(|reason| IOFileFormatExtensionSnafu { reason }.build())?;

        let file = File::open(&path).context(OpenFileSnafu { path: path.clone() })?;

        match file_ext {
            IOFileFormat::Csv => queries_from_csv(file),
            IOFileFormat::Json => queries_from_json(file),
        }
        .context(DeserDatasetsFromFileSnafu { path: path.clone() })
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
        let extra_tags: Vec<Tag> = extra_tags
            .iter()
            .map(|t| t.selector.last_tag())
            .filter(is_output_tag)
            .collect();
        match &mut self.tags {
            TagScope::Shared(vec_of_tags) => push_unique_tags(vec_of_tags, &extra_tags),
            TagScope::PerQuery(vec_vec_of_tags) => vec_vec_of_tags
                .iter_mut()
                .for_each(|vt| push_unique_tags(vt, &extra_tags)),
        }
    }

    pub fn validate_required_tags(&self) -> Result<(), Error> {
        let has_value =
            |ds: &InMemDicomObject, tag: Tag| ds.get(tag).is_some_and(|e| !e.is_empty());

        for (index, ds) in self.queries.iter().enumerate() {
            ensure!(
                has_value(ds, STUDY_INSTANCE_UID)
                    | (has_value(ds, PATIENT_ID) & has_value(ds, STUDY_DATE)),
                NoRequiredIdentifierSnafu { index }
            );
        }
        Ok(())
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
            TagScope::Shared(vec_of_tags) => vec_of_tags.clone(),
            TagScope::PerQuery(vec_vec_of_tags) => {
                let mut seen = HashSet::new();
                vec_vec_of_tags
                    .iter()
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
        TagScope::Shared(vec_of_tags) => vec_of_tags,
        TagScope::PerQuery(vec_vec_of_tags) => &vec_vec_of_tags[i],
    }
}
pub fn build_queries(
    file: Option<PathBuf>,
    term_tags: Vec<TermQuery>,
    _verbose: bool,
) -> Result<DicomQuerySet, Error> {
    let mut query_datasets = match file {
        Some(file) => DicomQuerySet::queries_from_file(file)?,
        None => DicomQuerySet::default(),
    };

    query_datasets.add_terminal_tags(&term_tags)?;

    query_datasets.validate_required_tags()?;

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
