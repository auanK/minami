use std::path::Path;

use crate::{
    InspectionReport, MinamiError,
    epub::{archive, navigation, package},
};

pub fn inspect_epub(path: &Path) -> Result<InspectionReport, MinamiError> {
    let mut archive = archive::open(path)?;
    let package_path = package::locate(&mut archive)?;
    let package = package::parse(&mut archive, &package_path)?;
    let navigation = navigation::parse(&mut archive, &package.ncx_path, &package.resources)?;
    Ok(InspectionReport {
        epub_version: package.version,
        package_path,
        resources: package.resources,
        spine: package.spine,
        navigation,
    })
}
