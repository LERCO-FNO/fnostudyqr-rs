use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dicom_core::value::{DicomDate, DicomDateTime, DicomTime};
use snafu::{ResultExt, Whatever, whatever};
use std::path::PathBuf;
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

pub fn validate_response_filepath(value: &str) -> Result<PathBuf, Whatever> {
    let path = PathBuf::from(value);
    if !path.exists() {
        whatever!("Response path doesn't exist");
    }
    Ok(path)
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

// TODO: possibly add parse_datetime_range()?
// TODO: possibly add parse_time-range()? is it needed?
