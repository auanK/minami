use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct InspectionReport {
    pub epub_version: String,
    pub package_path: String,
    pub resources: Vec<ManifestResource>,
    pub spine: Vec<SpineEntry>,
    pub navigation: Vec<NavigationEntry>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ManifestResource {
    pub id: String,
    pub path: String,
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpineEntry {
    pub idref: String,
    pub linear: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct NavigationEntry {
    pub id: Option<String>,
    pub title: String,
    pub path: String,
    pub fragment: Option<String>,
    /// Zero-based document order (preorder), independent of NCX playOrder.
    pub position: usize,
    /// Zero for a direct child of navMap.
    pub depth: usize,
}
