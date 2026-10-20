use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dicom_core::{
    Tag,
    value::{DicomDate, DicomDateTime, DicomTime},
};
use dicom_dictionary_std::tags::QUERY_RETRIEVE_LEVEL;
use snafu::{ResultExt, Whatever, whatever};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

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
    // accept only valid file paths with existing parent directory
    // directory or possible directory paths are not allowed
    let path_ref = Path::new(path);

    if path_ref.is_dir() || path.ends_with(['/', std::path::MAIN_SEPARATOR]) {
        whatever!("Directory path not allowed: `{}`", path_ref.display());
    } else if path_ref.extension().is_none() {
        // in case required extension is missing or path is
        // non-existing directory
        whatever!(
            "Path appears file like but has no extension: `{}`",
            path_ref.display()
        )
    }

    let parent = match path_ref.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => whatever!("Path `{}` has no parent", path_ref.display()),
    };

    if parent.is_dir() {
        std::path::absolute(path_ref).with_whatever_context(|e| format!("{e}"))
    } else {
        whatever!("Parent directory not found for `{}`", path);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_response_path() {
        let path = "./response.json";
        let res = validate_response_filepath(path);
        assert!(res.is_ok());

        let path = "/home/vojt/prace/fnostudyqr-rs/response.json";
        let res = validate_response_filepath(path);
        assert!(res.is_ok())
    }

    #[test]
    fn test_is_invalid_response_path() {
        let path = ".";
        let res = validate_response_filepath(path);
        assert!(res.is_err());

        let path = std::env::current_dir()
            .expect("Expected current directory")
            .to_str()
            .expect("Expected current directory as &str")
            .to_owned();
        let res = validate_response_filepath(&path);
        assert!(res.is_err());
    }

    #[test]
    fn test_is_valid_date_range() {
        let parsed = parse_date_range("2020-01-01..2020-05-05");
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), "20200101-20200505");
    }

    #[test]
    fn test_is_valid_time() {
        let parsed = parse_time("07:30:20.050");
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), "073020.050000")
    }

    #[test]
    fn test_is_valid_datetime() {
        let parsed = parse_datetime("2020-01-01 07:30:20");
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), "20200101073020.000000")
    }
}
