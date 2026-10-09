use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dicom_core::{
    Tag,
    value::{DicomDate, DicomDateTime, DicomTime},
};
use dicom_dictionary_std::tags::QUERY_RETRIEVE_LEVEL;
use snafu::{OptionExt, ResultExt, Whatever, whatever};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tracing::warn;

use crate::FileExtension;

pub fn parse_date(date_str: &str) -> Result<String, Whatever> {
    let date = NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d")
        .with_whatever_context(|e| format!("Invalid date format: {e}"))?;
    let dicom_dt = DicomDate::try_from(&date).whatever_context("Failed converting to DicomDate")?;
    Ok(dicom_dt.to_encoded())
}

pub fn parse_date_range(date_range_str: &str) -> Result<String, Whatever> {
    match date_range_str.split_once("..") {
        Some((start, end)) => Ok(format!("{}-{}", parse_date(start)?, parse_date(end)?)),
        None => parse_date(date_range_str),
    }
}

pub fn parse_time(time_str: &str) -> Result<String, Whatever> {
    let time = NaiveTime::parse_from_str(time_str, "%H:%M:%S%.f")
        .with_whatever_context(|e| format!("Invalid time format: {e}"))?;
    let dicom_time = DicomTime::try_from(&time)
        .with_whatever_context(|e| format!("Failed converting to DicomTime: {e}"))?;
    Ok(dicom_time.to_encoded())
}

pub fn parse_datetime(datetime_str: &str) -> Result<String, Whatever> {
    let datetime = NaiveDateTime::parse_from_str(datetime_str.trim(), "%Y-%m-%d %H:%M:%S%.f")
        .with_whatever_context(|e| format!("Invalid datetime format: {e}"))?;
    let dicom_dt = DicomDateTime::try_from(&datetime)
        .whatever_context("Failed converting to DicomDateTime")?;
    Ok(dicom_dt.to_encoded())
}

pub fn validate_response_filepath(path: &str) -> Result<PathBuf, Whatever> {
    let path_ref = Path::new(path);

    // - case 1: path is dir -> check directory exists -> will write as path/to/dir/response.<extension>
    if path_ref.is_dir() {
        return to_absolute_path(path_ref);
    }

    // - case 2: path is file -> check parent directory exists -> will write as path/to/parent/<filename>.<extension>
    if path.ends_with(['/', std::path::MAIN_SEPARATOR]) || path_ref.extension().is_none() {
        whatever!("Directory {path_ref:?} not found")
    }

    let parent = match path_ref.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => whatever!("Path {path_ref:?} has no parent"),
    };

    if !parent.is_dir() {
        whatever!("Parent {parent:?} is not a directory");
    }

    let file_name = path_ref
        .file_name()
        .whatever_context(format!("Invalid path {path_ref:?}"))?;
    Ok(to_absolute_path(parent)?.join(file_name))
}

pub fn construct_filepath(path: PathBuf, extension: FileExtension) -> PathBuf {
    let ext = match extension {
        FileExtension::Csv => "csv",
        FileExtension::Json => "json",
    };
    if path.is_dir() {
        path.join("response").with_extension(ext)
    } else {
        if let Some(extension) = path.extension()
            && (extension.to_os_string() != ext)
        {
            warn!(
                "--response-path extension {} different from --response-extension {}",
                extension.display(),
                ext
            )
        }
        path
    }
}

fn to_absolute_path(path: &Path) -> Result<PathBuf, Whatever> {
    std::path::absolute(path).with_whatever_context(|e| format!("{e}"))
}

pub fn push_unique_tags(current: &mut Vec<Tag>, extra_tags: &[Tag]) {
    let mut seen: HashSet<Tag> = current.iter().copied().collect();
    current.extend(extra_tags.iter().copied().filter(|t| seen.insert(*t)));
}

pub fn is_output_tag(t: &Tag) -> bool {
    ![QUERY_RETRIEVE_LEVEL].contains(t)
}

// TODO: possibly add parse_datetime_range()?
// TODO: possibly add parse_time-range()? is it needed?
