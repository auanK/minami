pub mod application;
mod epub;
pub mod error;
pub mod model;

pub use application::inspect::inspect_epub;
pub use error::MinamiError;
pub use model::{InspectionReport, ManifestResource, NavigationEntry, SpineEntry};
