use std::collections::{BTreeMap, BTreeSet};

use super::{
    NCX_NS, XHTML, ambiguous,
    archive::{EpubArchive, resolve},
    attribute, child, invalid, missing, parse_xml, unsupported,
};
use crate::{ManifestResource, MinamiError, NavigationEntry};

pub(crate) fn parse(
    archive: &mut EpubArchive,
    path: &str,
    resources: &[ManifestResource],
) -> Result<Vec<NavigationEntry>, MinamiError> {
    let text = archive.read_xml(path)?;
    let doc = parse_xml(&text, path)?;
    let root = doc.root_element();
    if !root.has_tag_name((NCX_NS, "ncx")) {
        return Err(invalid(path, "expected NCX root and namespace"));
    }
    if attribute(root, "version", path)? != "2005-1" {
        return Err(unsupported(path, "NCX version other than 2005-1"));
    }
    let nav_map = child(root, NCX_NS, "navMap", path)?;
    let by_path: BTreeMap<_, _> = resources
        .iter()
        .map(|r| (r.path.as_str(), r.media_type.as_str()))
        .collect();
    let mut ids = BTreeSet::new();
    let mut navigation = Vec::new();
    for node in nav_map.descendants().filter(|n| n.is_element()) {
        if node == nav_map {
            continue;
        }
        if !node.has_tag_name((NCX_NS, "navPoint")) {
            let parent_is_navpoint = node
                .parent()
                .is_some_and(|p| p.has_tag_name((NCX_NS, "navPoint")));
            if node.parent() == Some(nav_map)
                || (parent_is_navpoint
                    && !node.has_tag_name((NCX_NS, "navLabel"))
                    && !node.has_tag_name((NCX_NS, "content")))
            {
                return Err(unsupported(
                    path,
                    format!("navigation element {}", node.tag_name().name()),
                ));
            }
            continue;
        }
        if !node
            .parent()
            .is_some_and(|p| p == nav_map || p.has_tag_name((NCX_NS, "navPoint")))
        {
            return Err(invalid(path, "navPoint outside navigation hierarchy"));
        }
        let depth = node
            .ancestors()
            .skip(1)
            .take_while(|n| *n != nav_map)
            .count();
        if depth > 64 {
            return Err(MinamiError::LimitExceeded {
                resource: path.into(),
                limit: "64 nested NCX levels",
            });
        }
        let id = node.attribute("id");
        if let Some(id) = id {
            if id.trim().is_empty() {
                return Err(invalid(path, "empty navPoint id"));
            }
            if !ids.insert(id) {
                return Err(ambiguous(path, format!("duplicate navPoint id {id}")));
            }
        }
        if let Some(order) = node.attribute("playOrder")
            && !order.parse::<u32>().is_ok_and(|n| n > 0)
        {
            return Err(invalid(path, format!("invalid playOrder {order}")));
        }
        let label = child(node, NCX_NS, "navLabel", path)?;
        let title_node = child(label, NCX_NS, "text", path)?;
        let title: String = title_node
            .descendants()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect();
        if title.trim().is_empty() {
            return Err(missing(path, "navLabel text"));
        }
        let content = child(node, NCX_NS, "content", path)?;
        let src = attribute(content, "src", path)?;
        let (target, fragment) = resolve(path, src)?;
        match by_path.get(target.as_str()) {
            None => {
                return Err(invalid(
                    path,
                    format!("NCX target is not in manifest: {target}"),
                ));
            }
            Some(&XHTML) => {}
            Some(media_type) => {
                return Err(unsupported(
                    path,
                    format!("NCX target {target} has media type {media_type}"),
                ));
            }
        }
        archive.verify(&target)?;
        navigation.push(NavigationEntry {
            id: id.map(str::to_owned),
            title: title.trim().into(),
            path: target,
            fragment,
            position: navigation.len(),
            depth,
        });
    }
    if navigation.is_empty() {
        return Err(missing(path, "navPoint entries"));
    }
    Ok(navigation)
}
