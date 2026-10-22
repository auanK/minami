use std::collections::BTreeMap;

use serde::Serialize;
use xmlparser::{ElementEnd, ExternalId, Token, Tokenizer};

use crate::MinamiError;

pub(crate) const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ByteSpan(pub usize, pub usize);

#[derive(Debug)]
pub(crate) struct TextChunk {
    pub span: ByteSpan,
    pub text: String,
}

#[derive(Debug)]
pub(crate) struct Element {
    pub name: String,
    pub namespace: String,
    pub id: Option<String>,
    pub href: bool,
    pub open: ByteSpan,
    pub close: Option<ByteSpan>,
}

#[derive(Debug)]
pub(crate) enum Kind {
    Root,
    Element(Element),
    Text(Vec<TextChunk>),
    Comment,
    Pi,
}

#[derive(Debug)]
pub(crate) struct Node {
    pub span: ByteSpan,
    pub kind: Kind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl Node {
    pub fn element(&self) -> Option<&Element> {
        match &self.kind {
            Kind::Element(element) => Some(element),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Document {
    pub nodes: Vec<Node>,
    pub body: usize,
    pub ids: BTreeMap<String, usize>,
}

pub(crate) fn protected_space(c: char) -> bool {
    matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
}

pub(crate) fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn malformed(path: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::MalformedXml {
        resource: path.into(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            detail.into(),
        )),
    }
}

fn limit(path: &str, limit: &'static str) -> MinamiError {
    MinamiError::LimitExceeded {
        resource: path.into(),
        limit,
    }
}

pub(crate) fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn decode(
    raw: &str,
    start: usize,
    references: bool,
    nbsp: bool,
    replacements: &mut Vec<usize>,
    path: &str,
) -> Result<Vec<TextChunk>, MinamiError> {
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let begin = offset;
        let c = raw[offset..]
            .chars()
            .next()
            .ok_or_else(|| malformed(path, "missing character"))?;
        let value = if c == '&' && references {
            let end = raw[offset..]
                .find(';')
                .map(|n| n + offset)
                .ok_or_else(|| malformed(path, "unterminated entity reference"))?;
            let name = &raw[offset + 1..end];
            offset = end + 1;
            match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "apos" => '\'',
                "quot" => '"',
                "nbsp" if nbsp => {
                    replacements.push(start + begin);
                    '\u{a0}'
                }
                _ => {
                    let number = if let Some(hex) = name.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(decimal) = name.strip_prefix('#') {
                        // XML references have digits only; Rust's numeric parser also accepts '+'.
                        if decimal.bytes().all(|b| b.is_ascii_digit()) {
                            decimal.parse().ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    number
                        .and_then(char::from_u32)
                        .filter(|c| xml_char(*c))
                        .ok_or_else(|| {
                            malformed(path, format!("invalid or unsupported entity &{name};"))
                        })?
                }
            }
        } else {
            offset += c.len_utf8();
            if c == '\r' {
                if raw.as_bytes().get(offset) == Some(&b'\n') {
                    offset += 1;
                }
                '\n'
            } else {
                c
            }
        };
        if protected_space(value) {
            chunks.push(TextChunk {
                span: ByteSpan(start + begin, start + offset),
                text: value.to_string(),
            });
        } else if let Some(last) = chunks
            .last_mut()
            .filter(|last| !last.text.chars().next().is_some_and(protected_space))
        {
            last.text.push(value);
            last.span.1 = start + offset;
        } else {
            chunks.push(TextChunk {
                span: ByteSpan(start + begin, start + offset),
                text: value.to_string(),
            });
        }
        if chunks.len() > MAX_NODES {
            return Err(limit(path, "100000 XHTML text chunks"));
        }
    }
    Ok(chunks)
}

pub(crate) fn parse(source: &str, path: &str) -> Result<Document, MinamiError> {
    if source.len() > MAX_BYTES {
        return Err(limit(path, "32 MiB XHTML document"));
    }
    if source.chars().any(|c| !xml_char(c)) {
        return Err(malformed(path, "invalid XML character"));
    }
    let mut nodes = vec![Node {
        span: ByteSpan(0, source.len()),
        kind: Kind::Root,
        parent: None,
        children: Vec::new(),
    }];
    let mut stack = vec![0usize];
    let mut pending = None;
    let mut nbsp = false;
    let mut replacements = Vec::new();
    let mut chunk_count = 0usize;
    let mut attribute_count = 0usize;
    let mut element_attributes = 0usize;
    for result in Tokenizer::from(source) {
        let token = result.map_err(|source| MinamiError::MalformedXml {
            resource: path.into(),
            source: Box::new(source),
        })?;
        let mut new_node = None;
        match token {
            Token::Declaration {
                version, encoding, ..
            } => {
                if version.as_str() != "1.0"
                    || encoding.is_some_and(|e| !e.as_str().eq_ignore_ascii_case("UTF-8"))
                {
                    return Err(super::unsupported(
                        path,
                        "expected XML 1.0 with UTF-8 encoding",
                    ));
                }
            }
            Token::EmptyDtd {
                name, external_id, ..
            } => {
                nbsp = name.as_str() == "html"
                    && matches!(external_id, Some(ExternalId::Public(public, system)) if public.as_str() == "-//W3C//DTD XHTML 1.1//EN" && matches!(system.as_str(), "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd" | "https://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"));
                if !nbsp {
                    return Err(super::unsupported(
                        path,
                        "DOCTYPE is not the supported XHTML 1.1 declaration",
                    ));
                }
            }
            Token::DtdStart { .. } | Token::DtdEnd { .. } | Token::EntityDeclaration { .. } => {
                return Err(super::unsupported(
                    path,
                    "internal DTD or entity declaration",
                ));
            }
            Token::ElementStart { local, span, .. } => {
                element_attributes = 0;
                pending = Some(nodes.len());
                new_node = Some(Node {
                    span: ByteSpan(span.start(), span.end()),
                    kind: Kind::Element(Element {
                        name: local.as_str().into(),
                        namespace: String::new(),
                        id: None,
                        href: false,
                        open: ByteSpan(span.start(), span.end()),
                        close: None,
                    }),
                    parent: stack.last().copied(),
                    children: Vec::new(),
                });
            }
            Token::Attribute { value, .. } => {
                attribute_count += 1;
                element_attributes += 1;
                if element_attributes > 256 || attribute_count > MAX_NODES {
                    return Err(limit(
                        path,
                        "256 attributes per element or 100000 per XHTML",
                    ));
                }
                let chunks = decode(
                    value.as_str(),
                    value.start(),
                    true,
                    nbsp,
                    &mut replacements,
                    path,
                )?;
                chunk_count += chunks.len();
            }
            Token::ElementEnd { end, span } => match end {
                ElementEnd::Open | ElementEnd::Empty => {
                    let index = pending
                        .take()
                        .ok_or_else(|| malformed(path, "tag end without start"))?;
                    if let Kind::Element(element) = &mut nodes[index].kind {
                        element.open.1 = span.end();
                    }
                    nodes[index].span.1 = span.end();
                    if end == ElementEnd::Open {
                        if stack.len() >= MAX_DEPTH {
                            return Err(limit(path, "128 XHTML nesting levels"));
                        }
                        stack.push(index);
                    }
                }
                ElementEnd::Close(_, _) => {
                    if stack.len() <= 1 {
                        return Err(malformed(path, "unexpected closing tag"));
                    }
                    let index = stack
                        .pop()
                        .ok_or_else(|| malformed(path, "missing open tag"))?;
                    nodes[index].span.1 = span.end();
                    if let Kind::Element(element) = &mut nodes[index].kind {
                        element.close = Some(ByteSpan(span.start(), span.end()));
                    }
                }
            },
            Token::Text { text } => {
                let chunks = decode(
                    text.as_str(),
                    text.start(),
                    true,
                    nbsp,
                    &mut replacements,
                    path,
                )?;
                chunk_count += chunks.len();
                new_node = Some(Node {
                    span: ByteSpan(text.start(), text.end()),
                    kind: Kind::Text(chunks),
                    parent: stack.last().copied(),
                    children: Vec::new(),
                });
            }
            Token::Cdata { text, span } => {
                let chunks = decode(
                    text.as_str(),
                    text.start(),
                    false,
                    false,
                    &mut replacements,
                    path,
                )?;
                chunk_count += chunks.len();
                new_node = Some(Node {
                    span: ByteSpan(span.start(), span.end()),
                    kind: Kind::Text(chunks),
                    parent: stack.last().copied(),
                    children: Vec::new(),
                });
            }
            Token::Comment { span, .. } | Token::ProcessingInstruction { span, .. } => {
                new_node = Some(Node {
                    span: ByteSpan(span.start(), span.end()),
                    kind: if matches!(token, Token::Comment { .. }) {
                        Kind::Comment
                    } else {
                        Kind::Pi
                    },
                    parent: stack.last().copied(),
                    children: Vec::new(),
                });
            }
        }
        if let Some(node) = new_node {
            let index = nodes.len();
            if let Some(parent) = node.parent {
                nodes[parent].children.push(index);
            }
            nodes.push(node);
        }
        if nodes.len() > MAX_NODES || chunk_count > MAX_NODES {
            return Err(limit(path, "100000 XHTML nodes/text chunks"));
        }
    }
    // Only lexical text/attribute references are replaced, with six bytes for six bytes.
    // Comments, CDATA and every original byte coordinate remain untouched.
    let mut normalized = source.as_bytes().to_vec();
    for offset in replacements {
        normalized[offset..offset + 6].copy_from_slice(b"&#160;");
    }
    let normalized = String::from_utf8(normalized).map_err(|source| MinamiError::MalformedXml {
        resource: path.into(),
        source: Box::new(source),
    })?;
    let validation = roxmltree::Document::parse_with_options(
        &normalized,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            entity_resolver: None,
            nodes_limit: MAX_NODES as u32,
        },
    )
    .map_err(|source| MinamiError::MalformedXml {
        resource: path.into(),
        source: Box::new(source),
    })?;
    if !validation.root_element().has_tag_name((XHTML_NS, "html")) {
        return Err(super::invalid(path, "expected html in the XHTML namespace"));
    }
    for child in validation.root_element().children() {
        if child.is_element()
            && !matches!(
                (child.tag_name().namespace(), child.tag_name().name()),
                (Some(XHTML_NS), "head" | "body")
            )
        {
            return Err(super::unsupported(
                path,
                "unclassified element outside XHTML head/body",
            ));
        }
        if child.is_text() && child.text().is_some_and(|t| !t.chars().all(xml_space)) {
            return Err(super::unsupported(
                path,
                "unclassified text outside XHTML head/body",
            ));
        }
    }
    let by_start: BTreeMap<_, _> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.element().is_some())
        .map(|(i, n)| (n.span.0, i))
        .collect();
    let mut ids = BTreeMap::new();
    let mut bodies = Vec::new();
    for node in validation.descendants().filter(|n| n.is_element()) {
        let index = *by_start
            .get(&node.range().start)
            .ok_or_else(|| malformed(path, "lexical element position mismatch"))?;
        let Kind::Element(element) = &mut nodes[index].kind else {
            return Err(malformed(path, "missing lexical element"));
        };
        element.namespace = node.tag_name().namespace().unwrap_or("").into();
        element.href = node.attribute("href").is_some();
        if node.attribute((roxmltree::NS_XML_URI, "base")).is_some() {
            return Err(super::unsupported(path, "xml:base"));
        }
        if element.namespace == XHTML_NS {
            if node.attribute((roxmltree::NS_XML_URI, "space")) == Some("preserve") {
                return Err(super::unsupported(path, "xml:space=preserve"));
            }
            if let Some(id) = node.attribute("id") {
                if id.trim().is_empty() {
                    return Err(super::invalid(path, "empty XHTML id"));
                }
                if ids.insert(id.to_owned(), index).is_some() {
                    return Err(super::ambiguous(path, format!("duplicate XHTML id {id}")));
                }
                element.id = Some(id.into());
            }
            if element.name == "body" {
                if node.parent() != Some(validation.root_element()) {
                    return Err(super::invalid(path, "body must be a direct child of html"));
                }
                bodies.push(index);
            }
        }
    }
    if bodies.len() != 1 {
        return Err(super::invalid(path, "expected exactly one XHTML body"));
    }
    Ok(Document {
        nodes,
        body: bodies[0],
        ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!("<html xmlns=\"{XHTML_NS}\"><body>{body}</body></html>")
    }

    fn text_chunks(doc: &Document) -> Vec<&TextChunk> {
        doc.nodes
            .iter()
            .flat_map(|n| match &n.kind {
                Kind::Text(chunks) => chunks.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn slice(source: &str, span: ByteSpan) -> &str {
        &source[span.0..span.1]
    }

    #[test]
    fn l01_utf8_offsets() {
        let src = format!("\u{feff}{}", wrap("<p>Café ☕</p>"));
        let doc = parse(&src, "body.xhtml").unwrap();
        let chunks = text_chunks(&doc);
        assert_eq!(chunks[0].text, "Café ☕");
        assert_eq!(slice(&src, chunks[0].span), "Café ☕");
        assert!(chunks[0].span.1 - chunks[0].span.0 > chunks[0].text.chars().count());
    }

    #[test]
    fn l02_numeric_entities_preserve_source_spans() {
        let src = wrap("<p>Caf&#233;&#x202f;tea &lt; &amp; &apos; &quot; &gt;</p>");
        let doc = parse(&src, "body.xhtml").unwrap();
        let chunks = text_chunks(&doc);
        assert_eq!(chunks[0].text, "Café");
        assert_eq!(slice(&src, chunks[0].span), "Caf&#233;");
        assert_eq!(chunks[1].text, "\u{202f}");
        assert_eq!(slice(&src, chunks[1].span), "&#x202f;");
        assert_eq!(chunks[2].text, "tea < & ' \" >");
    }

    #[test]
    fn l03_doctype_nbsp_is_contextual() {
        let declaration = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">";
        let src = format!("{declaration}{}", wrap("<p>A&nbsp;B</p>"));
        let doc = parse(&src, "body.xhtml").unwrap();
        let chunks = text_chunks(&doc);
        assert_eq!(chunks[1].text, "\u{a0}");
        assert_eq!(slice(&src, chunks[1].span), "&nbsp;");
        assert!(parse(&wrap("<p>&nbsp;</p>"), "body.xhtml").is_err());
    }

    #[test]
    fn l04_cdata_comments_and_pi_are_lexical() {
        let src = wrap("<p><![CDATA[A&nbsp;<x>]]><!--&nbsp;--><?keep &nbsp;?></p>");
        let doc = parse(&src, "body.xhtml").unwrap();
        assert_eq!(text_chunks(&doc)[0].text, "A&nbsp;<x>");
        assert!(
            doc.nodes
                .iter()
                .any(|n| matches!(n.kind, Kind::Comment) && slice(&src, n.span) == "<!--&nbsp;-->")
        );
        assert!(
            doc.nodes
                .iter()
                .any(|n| matches!(n.kind, Kind::Pi) && slice(&src, n.span) == "<?keep &nbsp;?>")
        );
    }

    #[test]
    fn l05_crlf_semantics_keep_original_coordinates() {
        let src = wrap("<p>A\r\nB\rC&#13;D</p>");
        let doc = parse(&src, "body.xhtml").unwrap();
        assert_eq!(text_chunks(&doc)[0].text, "A\nB\nC\rD");
        assert_eq!(slice(&src, text_chunks(&doc)[0].span), "A\r\nB\rC&#13;D");
    }

    #[test]
    fn l06_tags_attributes_empty_nodes_and_body() {
        let src = wrap("<p><i id='a' class=\"x>y\">Hi</i><a id=\"empty\"/></p>");
        let doc = parse(&src, "body.xhtml").unwrap();
        let i = &doc.nodes[doc.ids["a"]];
        let e = i.element().unwrap();
        assert_eq!(slice(&src, e.open), "<i id='a' class=\"x>y\">");
        assert_eq!(slice(&src, e.close.unwrap()), "</i>");
        assert_eq!(
            slice(&src, doc.nodes[doc.ids["empty"]].span),
            "<a id=\"empty\"/>"
        );
        let body = doc.nodes[doc.body].element().unwrap();
        assert_eq!(
            slice(&src, ByteSpan(body.open.1, body.close.unwrap().0)),
            "<p><i id='a' class=\"x>y\">Hi</i><a id=\"empty\"/></p>"
        );
    }

    #[test]
    fn l07_malformed_xml_and_invalid_characters() {
        for body in [
            "<p>bad</i>",
            "<p>truncated",
            "<p>&#0;</p>",
            "<p>\0</p>",
            "<p>&unknown;</p>",
            "<p x='1' x='2'/>",
        ] {
            assert!(
                matches!(
                    parse(&wrap(body), "bad.xhtml"),
                    Err(MinamiError::MalformedXml { .. })
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn l08_external_entities_and_unsupported_declarations() {
        for declaration in [
            "<!DOCTYPE html [<!ENTITY x SYSTEM 'file:///secret'>]>",
            "<!DOCTYPE html SYSTEM 'https://invalid.example/dtd'>",
            "<?xml version='1.0' encoding='ISO-8859-1'?>",
            "<?xml version='1.1'?>",
        ] {
            assert!(matches!(
                parse(
                    &format!("{declaration}{}", wrap("<p>text</p>")),
                    "bad.xhtml"
                ),
                Err(MinamiError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn l09_namespaces_body_and_ids() {
        for src in [
            "<html><body/></html>".into(),
            format!("<html xmlns='{XHTML_NS}'><head/></html>"),
            wrap("<body/>"),
            wrap("<p id='same'/><p id='same'/>"),
            wrap("<p id=' '/>"),
            wrap("<p xml:base='x'>Hi</p>"),
        ] {
            assert!(parse(&src, "bad.xhtml").is_err(), "{src}");
        }
        assert!(
            parse(
                &format!("<html xmlns='{XHTML_NS}'><x:body xmlns:x='urn:foreign'/></html>"),
                "bad.xhtml"
            )
            .is_err()
        );
    }

    #[test]
    fn l10_resource_limits() {
        let deep = wrap(&format!(
            "{}x{}",
            "<div>".repeat(MAX_DEPTH),
            "</div>".repeat(MAX_DEPTH)
        ));
        assert!(matches!(
            parse(&deep, "deep.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let large = " ".repeat(MAX_BYTES + 1);
        assert!(matches!(
            parse(&large, "large.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let many = wrap(&"<p/>".repeat(MAX_NODES));
        assert!(matches!(
            parse(&many, "many.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn l11_attribute_budgets_are_bounded_before_dom_validation() {
        let attrs = (0..300).map(|n| format!(" a{n}='' ")).collect::<String>();
        assert!(matches!(
            parse(&wrap(&format!("<p{attrs}/>")), "wide.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn l12_unclassified_root_content_is_not_silently_ignored() {
        for extra in [
            "<other:body xmlns:other='urn:foreign'>Lost</other:body>",
            "<p>Lost</p>",
            "Lost",
        ] {
            let src = format!("<html xmlns='{XHTML_NS}'><body/>{extra}</html>");
            assert!(parse(&src, "root.xhtml").is_err(), "{extra}");
        }
    }

    #[test]
    fn l13_total_attribute_budget_is_not_only_per_element() {
        let attrs = (0..200).map(|n| format!(" a{n}='' ")).collect::<String>();
        let source = wrap(&format!("<p{attrs}/>").repeat(501));
        assert!(matches!(
            parse(&source, "attributes.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
    }
}
