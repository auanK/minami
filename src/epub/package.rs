use std::collections::{BTreeMap, BTreeSet};

use super::{
    NCX, OPF_NS, XHTML, ambiguous,
    archive::{EpubArchive, resolve},
    attribute, child, invalid, missing, parse_xml, unsupported,
};
use crate::{ManifestResource, MinamiError, SpineEntry};

pub(crate) struct Package {
    pub version: String,
    pub resources: Vec<ManifestResource>,
    pub spine: Vec<SpineEntry>,
    pub ncx_path: String,
}

pub(crate) fn locate(archive: &mut EpubArchive) -> Result<String, MinamiError> {
    const PATH: &str = "META-INF/container.xml";
    const NS: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
    let text = archive.read_xml(PATH)?;
    let doc = parse_xml(&text, PATH)?;
    let root = doc.root_element();
    if !root.has_tag_name((NS, "container")) {
        return Err(invalid(PATH, "expected EPUB container root and namespace"));
    }
    let rootfiles = child(root, NS, "rootfiles", PATH)?;
    let mut supported = rootfiles.children().filter(|n| {
        n.has_tag_name((NS, "rootfile"))
            && n.attribute("media-type") == Some("application/oebps-package+xml")
    });
    let rootfile = supported
        .next()
        .ok_or_else(|| missing(PATH, "supported rootfile"))?;
    if supported.next().is_some() {
        return Err(ambiguous(PATH, "multiple supported package rootfiles"));
    }
    let reference = attribute(rootfile, "full-path", PATH)?;
    let (path, fragment) = resolve("", reference)?;
    if fragment.is_some() || reference.split('/').any(|s| matches!(s, "." | "..")) {
        return Err(invalid(PATH, format!("invalid rootfile path: {reference}")));
    }
    if !archive.contains(&path) {
        return Err(invalid(PATH, format!("missing package {path}")));
    }
    Ok(path)
}

pub(crate) fn parse(archive: &mut EpubArchive, path: &str) -> Result<Package, MinamiError> {
    let text = archive.read_xml(path)?;
    let doc = parse_xml(&text, path)?;
    let root = doc.root_element();
    if !root.has_tag_name((OPF_NS, "package")) {
        return Err(invalid(path, "expected OPF package root and namespace"));
    }
    let version = attribute(root, "version", path)?;
    if version != "2.0" {
        return Err(unsupported(
            path,
            format!("EPUB version {version}; expected 2.0"),
        ));
    }
    let manifest = child(root, OPF_NS, "manifest", path)?;
    let mut resources = Vec::new();
    let mut ids = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for item in manifest.children().filter(|n| n.is_element()) {
        if !item.has_tag_name((OPF_NS, "item")) {
            return Err(unsupported(path, "non-item manifest child"));
        }
        let id = attribute(item, "id", path)?;
        if ids.insert(id, resources.len()).is_some() {
            return Err(ambiguous(path, format!("duplicate manifest id {id}")));
        }
        let href = attribute(item, "href", path)?;
        let (resource_path, fragment) = resolve(path, href)?;
        if fragment.is_some() {
            return Err(invalid(
                path,
                format!("manifest href contains a fragment: {href}"),
            ));
        }
        if !paths.insert(resource_path.clone()) {
            return Err(ambiguous(
                path,
                format!("duplicate manifest path {resource_path}"),
            ));
        }
        if !archive.contains(&resource_path) {
            return Err(invalid(
                path,
                format!("missing manifest resource {id}: {resource_path}"),
            ));
        }
        resources.push(ManifestResource {
            id: id.into(),
            path: resource_path,
            media_type: attribute(item, "media-type", path)?.into(),
        });
    }
    if resources.is_empty() {
        return Err(missing(path, "manifest items"));
    }
    let spine_node = child(root, OPF_NS, "spine", path)?;
    let ncx_id = attribute(spine_node, "toc", path)?;
    let ncx = ids
        .get(ncx_id)
        .map(|&i| &resources[i])
        .ok_or_else(|| invalid(path, format!("spine toc {ncx_id} is not in manifest")))?;
    if ncx.media_type != NCX {
        return Err(unsupported(
            path,
            format!("spine toc {ncx_id} has media type {}", ncx.media_type),
        ));
    }
    let ncx_path = ncx.path.clone();
    let mut spine = Vec::new();
    for itemref in spine_node.children().filter(|n| n.is_element()) {
        if !itemref.has_tag_name((OPF_NS, "itemref")) {
            return Err(unsupported(path, "non-itemref spine child"));
        }
        let idref = attribute(itemref, "idref", path)?;
        let resource = ids
            .get(idref)
            .map(|&i| &resources[i])
            .ok_or_else(|| invalid(path, format!("spine idref {idref} is not in manifest")))?;
        if resource.media_type != XHTML {
            return Err(unsupported(
                path,
                format!(
                    "reading resource {idref} has media type {}",
                    resource.media_type
                ),
            ));
        }
        let linear = match itemref.attribute("linear") {
            None | Some("yes") => true,
            Some("no") => false,
            Some(value) => {
                return Err(invalid(
                    path,
                    format!("invalid linear value {value} for {idref}"),
                ));
            }
        };
        archive.verify(&resource.path)?;
        spine.push(SpineEntry {
            idref: idref.into(),
            linear,
        });
    }
    if spine.is_empty() {
        return Err(missing(path, "spine itemrefs"));
    }
    Ok(Package {
        version: version.into(),
        resources,
        spine,
        ncx_path,
    })
}
