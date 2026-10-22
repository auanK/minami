use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MinamiError {
    #[error("I/O while {operation} '{resource}': {source}")]
    Io {
        operation: &'static str,
        resource: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid ZIP '{resource}': {source}")]
    InvalidZip {
        resource: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("security limit for '{resource}': {limit}")]
    LimitExceeded {
        resource: String,
        limit: &'static str,
    },
    #[error("malformed XML in '{resource}': {source}")]
    MalformedXml {
        resource: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("missing structure in '{resource}': {detail}")]
    MissingStructure { resource: String, detail: String },
    #[error("invalid reference in '{resource}': {detail}")]
    InvalidReference { resource: String, detail: String },
    #[error("ambiguous structure in '{resource}': {detail}")]
    AmbiguousStructure { resource: String, detail: String },
    #[error("unsupported format in '{resource}': {detail}")]
    Unsupported { resource: String, detail: String },
}
