use dicom_core::{Tag, VR};
use snafu::prelude::*;
use std::path::PathBuf;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum DeserError {
    #[snafu(display("Could not open {}", path.display()))]
    OpenFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("Invalid JSON in {}", path.display()))]
    Json {
        source: serde_json::Error,
        path: PathBuf,
    },

    #[snafu(display("Invalid CSV in {}", source))]
    Csv { source: csv::Error },

    #[snafu(display("Invalid DICOM tag header: {}", header))]
    InvalidHeaderTag { header: String },

    #[snafu(display("Invalid VR {} for DICOM tag {}", vr, tag))]
    InvalidVR { tag: Tag, vr: VR },

    #[snafu(display("Bad value {:?} for tag {}", value, tag))]
    BadValue { value: String, tag: Tag },

    #[snafu(whatever, display("{}", message))]
    Other {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error + 'static>, Some)))]
        source: Option<Box<dyn std::error::Error + 'static>>,
    },
}
