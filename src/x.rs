use serde::Serialize;

use crate::epub::xhtml::ByteSpan;
use crate::{NavigationEntry, SpineEntry};

pub(crate) const VERSION: &str = "1.0";
pub(crate) const PROFILE: &str = "epub2-xhtml-v1";

#[derive(Debug, Serialize)]
pub(crate) struct Control {
    pub format_version: &'static str,
    pub profile: &'static str,
    pub extraction_id: String,
    pub source: Origin,
    pub spine: Vec<SpineEntry>,
    pub documents: Vec<Document>,
    pub navigation: Vec<NavigationEntry>,
    pub sections: Vec<Section>,
    pub segments: Vec<Segment>,
    pub preserved: Vec<Preserved>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Origin {
    pub epub_sha256: String,
    pub epub_version: String,
    pub package_path: String,
    pub ncx_path: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Document {
    pub id: String,
    pub manifest_id: String,
    pub path: String,
    pub body: ByteSpan,
}

#[derive(Debug, Serialize)]
pub(crate) struct Section {
    pub id: String,
    pub document_id: String,
    pub range: ByteSpan,
    pub navigation_positions: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Segment {
    pub id: String,
    pub document_id: String,
    pub section_id: String,
    pub range: ByteSpan,
    pub markers: Vec<Marker>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Preserved {
    pub document_id: String,
    pub range: ByteSpan,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct Payload {
    pub format_version: &'static str,
    pub extraction_id: String,
    pub segments: Vec<SourceSegment>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SourceSegment {
    pub id: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Part {
    Text(String),
    Open(String),
    Close(String),
    Atom(String),
}

#[derive(Debug, Serialize)]
pub(crate) struct Marker {
    pub id: String,
    #[serde(flatten)]
    pub token: Token,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Token {
    Pair {
        parent: Option<String>,
        open: ByteSpan,
        close: ByteSpan,
    },
    Atom {
        parent: Option<String>,
        range: ByteSpan,
    },
}
