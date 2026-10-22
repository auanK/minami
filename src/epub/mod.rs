pub(crate) mod archive;
pub(crate) mod navigation;
pub(crate) mod package;

use roxmltree::{Document, Node, ParsingOptions};

use crate::MinamiError;

pub(crate) const OPF_NS: &str = "http://www.idpf.org/2007/opf";
pub(crate) const NCX_NS: &str = "http://www.daisy.org/z3986/2005/ncx/";
pub(crate) const XHTML: &str = "application/xhtml+xml";
pub(crate) const NCX: &str = "application/x-dtbncx+xml";

pub(crate) fn missing(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::MissingStructure {
        resource: resource.into(),
        detail: detail.into(),
    }
}

pub(crate) fn invalid(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::InvalidReference {
        resource: resource.into(),
        detail: detail.into(),
    }
}

pub(crate) fn ambiguous(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::AmbiguousStructure {
        resource: resource.into(),
        detail: detail.into(),
    }
}

pub(crate) fn unsupported(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::Unsupported {
        resource: resource.into(),
        detail: detail.into(),
    }
}

pub(crate) fn parse_xml<'a>(text: &'a str, resource: &str) -> Result<Document<'a>, MinamiError> {
    // shortcut: even declaration-like text is rejected, revisit if a real EPUB needs custom entities.
    if text.contains("<!ENTITY") {
        return Err(unsupported(resource, "custom XML entities"));
    }
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            // EPUB 2 NCX declares an external DTD; no resolver means it is never fetched.
            allow_dtd: true,
            entity_resolver: None,
            nodes_limit: 100_000,
        },
    )
    .map_err(|source| match source {
        roxmltree::Error::NodesLimitReached => MinamiError::LimitExceeded {
            resource: resource.into(),
            limit: "100000 XML nodes",
        },
        _ => MinamiError::MalformedXml {
            resource: resource.into(),
            source: Box::new(source),
        },
    })?;
    if document
        .descendants()
        .any(|n| n.attribute((roxmltree::NS_XML_URI, "base")).is_some())
    {
        return Err(unsupported(resource, "xml:base"));
    }
    Ok(document)
}

pub(crate) fn child<'a, 'input>(
    parent: Node<'a, 'input>,
    ns: &str,
    name: &str,
    resource: &str,
) -> Result<Node<'a, 'input>, MinamiError> {
    let mut children = parent.children().filter(|n| n.has_tag_name((ns, name)));
    let node = children.next().ok_or_else(|| missing(resource, name))?;
    if children.next().is_some() {
        return Err(ambiguous(resource, format!("multiple {name} elements")));
    }
    Ok(node)
}

pub(crate) fn attribute<'a>(
    node: Node<'a, '_>,
    name: &str,
    resource: &str,
) -> Result<&'a str, MinamiError> {
    node.attribute(name)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| missing(resource, format!("{}@{name}", node.tag_name().name())))
}
