use std::collections::BTreeMap;

use super::xhtml::{ByteSpan, Document, Kind, XHTML_NS, protected_space, xml_space};
use crate::{
    MinamiError,
    x::{Marker, Part, Token},
};

#[derive(Debug)]
pub(crate) struct Draft {
    pub range: ByteSpan,
    pub markers: Vec<Marker>,
    pub parts: Vec<Part>,
}

#[derive(Debug)]
pub(crate) struct Preserved {
    pub range: ByteSpan,
    pub reason: &'static str,
}

#[derive(Debug)]
pub(crate) struct Segments {
    pub segments: Vec<Draft>,
    pub preserved: Vec<Preserved>,
    pub promotions: BTreeMap<usize, usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Flow,
    Structural,
    Inline,
    Atom,
    Boundary,
}

struct Analysis {
    classes: Vec<Class>,
    text: Vec<bool>,
}

fn contains_link(doc: &Document, index: usize) -> bool {
    let node = &doc.nodes[index];
    node.element()
        .is_some_and(|e| e.namespace == XHTML_NS && e.name == "a" && e.href)
        || node.children.iter().any(|i| contains_link(doc, *i))
}

fn structural_child(parent: &str, child: &str) -> bool {
    match parent {
        "ol" | "ul" => child == "li",
        "dl" => matches!(child, "dt" | "dd"),
        "table" => matches!(
            child,
            "caption" | "col" | "colgroup" | "thead" | "tbody" | "tfoot" | "tr"
        ),
        "thead" | "tbody" | "tfoot" => child == "tr",
        "tr" => matches!(child, "td" | "th"),
        "colgroup" => child == "col",
        _ => false,
    }
}

fn analyze(
    doc: &Document,
    index: usize,
    plan: &mut Analysis,
    path: &str,
) -> Result<bool, MinamiError> {
    let node = &doc.nodes[index];
    let Some(e) = node.element() else {
        let has_text = match &node.kind {
            Kind::Text(chunks) => chunks
                .iter()
                .any(|c| c.text.chars().any(|c| !xml_space(c) && !protected_space(c))),
            _ => false,
        };
        plan.text[index] = has_text;
        return Ok(has_text);
    };
    let class = if e.namespace == "http://www.w3.org/2000/svg" && e.name == "svg" {
        Class::Atom
    } else if e.namespace != XHTML_NS {
        return Err(super::unsupported(
            path,
            format!(
                "foreign element {{{}}}{} at byte {}",
                e.namespace, e.name, node.span.0
            ),
        ));
    } else {
        match e.name.as_str() {
            "body" | "div" | "center" | "blockquote" | "p" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" | "li" | "dt" | "dd" | "td" | "th" | "caption" => Class::Flow,
            "ol" | "ul" | "dl" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "colgroup" => {
                Class::Structural
            }
            "i" | "b" | "em" | "strong" | "span" | "a" | "sup" | "sub" | "big" | "small"
            | "font" => {
                if matches!(e.name.as_str(), "sup" | "sub") && contains_link(doc, index) {
                    Class::Atom
                } else {
                    Class::Inline
                }
            }
            "img" | "br" | "hr" | "col" => {
                if !node.children.is_empty() {
                    return Err(super::unsupported(
                        path,
                        format!("nonempty {} at byte {}", e.name, node.span.0),
                    ));
                }
                if matches!(e.name.as_str(), "hr" | "col") {
                    Class::Boundary
                } else {
                    Class::Atom
                }
            }
            "script" | "style" => Class::Boundary,
            _ => {
                return Err(super::unsupported(
                    path,
                    format!("element {} at byte {}", e.name, node.span.0),
                ));
            }
        }
    };
    plan.classes[index] = class;
    if matches!(class, Class::Atom | Class::Boundary) {
        return Ok(false);
    }
    let mut has_text = false;
    for &child in &node.children {
        let child_text = analyze(doc, child, plan, path)?;
        if class == Class::Inline
            && doc.nodes[child].element().is_some()
            && !matches!(plan.classes[child], Class::Inline | Class::Atom)
        {
            return Err(super::unsupported(
                path,
                format!(
                    "block/boundary inside inline {} at byte {}",
                    e.name, node.span.0
                ),
            ));
        }
        if class == Class::Structural {
            match &doc.nodes[child].kind {
                Kind::Element(c)
                    if c.namespace == XHTML_NS && structural_child(&e.name, &c.name) => {}
                Kind::Comment | Kind::Pi => {}
                Kind::Text(chunks) if chunks.iter().all(|c| c.text.chars().all(xml_space)) => {}
                _ => {
                    return Err(super::unsupported(
                        path,
                        format!(
                            "unexpected content inside {} at byte {}",
                            e.name, doc.nodes[child].span.0
                        ),
                    ));
                }
            }
        }
        has_text |= child_text;
    }
    plan.text[index] = has_text;
    Ok(has_text)
}

fn append_text(parts: &mut Vec<Part>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Part::Text(previous)) = parts.last_mut() {
        previous.push_str(text);
    } else {
        parts.push(Part::Text(text.into()));
    }
}

fn atom(draft: &mut Draft, role: &str, range: ByteSpan, parent: Option<&str>) {
    let id = format!("{role}{}", draft.markers.len() + 1);
    draft.parts.push(Part::Atom(id.clone()));
    draft.markers.push(Marker {
        id,
        token: Token::Atom {
            parent: parent.map(str::to_owned),
            range,
        },
    });
}

struct Emission<'a> {
    plan: &'a Analysis,
    draft: Draft,
    prefix: bool,
    promotions: &'a mut BTreeMap<usize, usize>,
}

fn emit(
    doc: &Document,
    index: usize,
    parent: Option<&str>,
    depth: usize,
    output: &mut Emission<'_>,
    path: &str,
) -> Result<(), MinamiError> {
    let node = &doc.nodes[index];
    match &node.kind {
        Kind::Text(chunks) => {
            for chunk in chunks {
                if chunk.text.chars().next().is_some_and(protected_space) {
                    atom(
                        &mut output.draft,
                        if chunk.text == "\u{a0}" {
                            "nbsp"
                        } else {
                            "space"
                        },
                        chunk.span,
                        parent,
                    );
                    output.prefix = false;
                } else {
                    append_text(&mut output.draft.parts, &chunk.text);
                    if !chunk.text.chars().all(xml_space) {
                        output.prefix = false;
                    }
                }
            }
        }
        Kind::Comment | Kind::Pi => {
            atom(
                &mut output.draft,
                if matches!(node.kind, Kind::Comment) {
                    "comment"
                } else {
                    "pi"
                },
                node.span,
                parent,
            );
            output.prefix = false;
        }
        Kind::Element(e) => {
            if output.prefix && output.plan.classes[index] == Class::Inline {
                output.promotions.insert(index, output.draft.range.0);
            }
            if output.plan.classes[index] == Class::Atom || !output.plan.text[index] {
                atom(&mut output.draft, &e.name, node.span, parent);
                output.prefix = false;
            } else {
                if depth >= 64 {
                    return Err(MinamiError::LimitExceeded {
                        resource: path.into(),
                        limit: "64 paired inline nesting levels",
                    });
                }
                let id = format!("{}{}", e.name, output.draft.markers.len() + 1);
                let close = e.close.ok_or_else(|| {
                    super::invalid(path, "text-bearing inline element has no closing tag")
                })?;
                output.draft.markers.push(Marker {
                    id: id.clone(),
                    token: Token::Pair {
                        parent: parent.map(str::to_owned),
                        open: e.open,
                        close,
                    },
                });
                output.draft.parts.push(Part::Open(id.clone()));
                for &child in &node.children {
                    emit(doc, child, Some(&id), depth + 1, output, path)?;
                }
                output.draft.parts.push(Part::Close(id));
            }
        }
        Kind::Root => return Err(super::invalid(path, "unexpected root in flow group")),
    }
    Ok(())
}

fn group(
    doc: &Document,
    indexes: &[usize],
    plan: &Analysis,
    output: &mut Segments,
    path: &str,
) -> Result<(), MinamiError> {
    let (Some(first), Some(last)) = (indexes.first(), indexes.last()) else {
        return Ok(());
    };
    let mut emission = Emission {
        plan,
        draft: Draft {
            range: ByteSpan(doc.nodes[*first].span.0, doc.nodes[*last].span.1),
            markers: Vec::new(),
            parts: Vec::new(),
        },
        prefix: true,
        promotions: &mut output.promotions,
    };
    for &index in indexes {
        emit(doc, index, None, 0, &mut emission, path)?;
    }
    if indexes.iter().any(|i| plan.text[*i]) {
        output.segments.push(emission.draft);
    } else {
        for marker in emission.draft.markers {
            if let Token::Atom { range, .. } = marker.token {
                output.preserved.push(Preserved {
                    range,
                    reason: if marker.id.starts_with("svg") {
                        "svg"
                    } else {
                        "atom"
                    },
                });
            }
        }
    }
    Ok(())
}

fn walk(
    doc: &Document,
    index: usize,
    plan: &Analysis,
    output: &mut Segments,
    path: &str,
) -> Result<(), MinamiError> {
    let mut current = Vec::new();
    for &child in &doc.nodes[index].children {
        if plan.classes[index] == Class::Flow
            && (doc.nodes[child].element().is_none()
                || matches!(plan.classes[child], Class::Inline | Class::Atom))
        {
            current.push(child);
            continue;
        }
        group(doc, &current, plan, output, path)?;
        current.clear();
        if let Some(element) = doc.nodes[child].element() {
            if matches!(plan.classes[child], Class::Flow | Class::Structural) {
                walk(doc, child, plan, output, path)?;
            } else if matches!(element.name.as_str(), "script" | "style") {
                output.preserved.push(Preserved {
                    range: doc.nodes[child].span,
                    reason: if element.name == "script" {
                        "script"
                    } else {
                        "style"
                    },
                });
            }
        } else if matches!(doc.nodes[child].kind, Kind::Comment | Kind::Pi) {
            output.preserved.push(Preserved {
                range: doc.nodes[child].span,
                reason: "atom",
            });
        }
    }
    group(doc, &current, plan, output, path)
}

pub(crate) fn extract(doc: &Document, path: &str) -> Result<Segments, MinamiError> {
    let mut plan = Analysis {
        classes: vec![Class::Atom; doc.nodes.len()],
        text: vec![false; doc.nodes.len()],
    };
    analyze(doc, doc.body, &mut plan, path)?;
    let mut output = Segments {
        segments: Vec::new(),
        preserved: Vec::new(),
        promotions: BTreeMap::new(),
    };
    for (index, node) in doc.nodes.iter().enumerate() {
        if matches!(plan.classes[index], Class::Flow | Class::Structural) {
            let start = if index == doc.body {
                node.element().map_or(node.span.0, |e| e.open.1)
            } else {
                node.span.0
            };
            output.promotions.insert(index, start);
        }
    }
    walk(doc, doc.body, &plan, &mut output, path)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{epub::xhtml, x::Token};
    use serde_json::{Value, json};

    fn extract_body(body: &str) -> (String, Segments) {
        let source = format!(
            "<html xmlns=\"{}\"><body>{body}</body></html>",
            xhtml::XHTML_NS
        );
        let doc = xhtml::parse(&source, "body.xhtml").unwrap();
        let result = extract(&doc, "body.xhtml").unwrap();
        (source, result)
    }

    fn parts(draft: &Draft) -> Value {
        serde_json::to_value(&draft.parts).unwrap()
    }

    #[test]
    fn s01_simple_paragraph() {
        let (src, out) = extract_body("<p>Hello.</p>");
        assert_eq!(out.segments.len(), 1);
        assert_eq!(parts(&out.segments[0]), json!([{"text":"Hello."}]));
        let range = out.segments[0].range;
        assert_eq!(&src[range.0..range.1], "Hello.");
    }

    #[test]
    fn s02_inline_text_is_one_segment() {
        let (_, out) = extract_body("<p>I <i>said</i> <b>this</b>.</p>");
        assert_eq!(out.segments.len(), 1);
        assert_eq!(
            parts(&out.segments[0]),
            json!([{"text":"I "},{"open":"i1"},{"text":"said"},{"close":"i1"},{"text":" "},{"open":"b2"},{"text":"this"},{"close":"b2"},{"text":"."}])
        );
    }

    #[test]
    fn s03_nested_markers_have_original_parents() {
        let (src, out) = extract_body("<p><b>Very <i>small</i></b> room.</p>");
        let markers = &out.segments[0].markers;
        assert_eq!(markers[0].id, "b1");
        assert_eq!(markers[1].id, "i2");
        let Token::Pair {
            parent,
            open,
            close,
        } = &markers[1].token
        else {
            panic!()
        };
        assert_eq!(parent.as_deref(), Some("b1"));
        assert_eq!(&src[open.0..open.1], "<i>");
        assert_eq!(&src[close.0..close.1], "</i>");
    }

    #[test]
    fn s04_link_attributes_stay_in_source() {
        let (src, out) =
            extract_body("<p>Read <a id='link' href='#note' class='x'>the note</a>.</p>");
        let m = &out.segments[0].markers[0];
        let Token::Pair { open, .. } = m.token else {
            panic!()
        };
        assert_eq!(&src[open.0..open.1], "<a id='link' href='#note' class='x'>");
        let encoded = serde_json::to_string(&out.segments[0].parts).unwrap();
        assert!(!encoded.contains("href") && !encoded.contains("#note"));
    }

    #[test]
    fn s05_direct_container_content_is_not_lost() {
        let (_, out) = extract_body("<div>Before<p>Inside</p>After<center>Centered</center></div>");
        let texts: Vec<_> = out.segments.iter().map(parts).collect();
        assert_eq!(texts,json!([[{"text":"Before"}],[{"text":"Inside"}],[{"text":"After"}],[{"text":"Centered"}]]).as_array().unwrap().clone());
    }

    #[test]
    fn s06_nested_tables_do_not_join_cells() {
        let (_, out) = extract_body(
            "<table><tr><th>Heading</th><td>First<table><tr><td>Nested</td></tr></table>Tail</td><td>Last</td></tr></table>",
        );
        let text: Vec<_> = out
            .segments
            .iter()
            .flat_map(|s| &s.parts)
            .filter_map(|p| {
                if let Part::Text(t) = p {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, ["Heading", "First", "Nested", "Tail", "Last"]);
    }

    #[test]
    fn s07_linked_superscript_is_one_atom_but_notes_are_text() {
        let (src, out) = extract_body(
            "<p>Word<sup id='ref'><a href='#note'>[1]</a></sup>.</p><ol><li id='note'>Footnote.</li></ol>",
        );
        assert_eq!(
            parts(&out.segments[0]),
            json!([{"text":"Word"},{"atom":"sup1"},{"text":"."}])
        );
        assert_eq!(out.segments[0].markers.len(), 1);
        let Token::Atom { range, .. } = out.segments[0].markers[0].token else {
            panic!()
        };
        assert_eq!(
            &src[range.0..range.1],
            "<sup id='ref'><a href='#note'>[1]</a></sup>"
        );
        assert_eq!(parts(&out.segments[1]), json!([{"text":"Footnote."}]));
    }

    #[test]
    fn s08_image_svg_comments_and_pi_are_protected() {
        let (_, out) = extract_body(
            "<p>A<img src='i.png'/><svg xmlns='http://www.w3.org/2000/svg'><text>Unedited</text></svg><!--keep--><?keep value?>Z</p>",
        );
        assert_eq!(
            parts(&out.segments[0]),
            json!([{"text":"A"},{"atom":"img1"},{"atom":"svg2"},{"atom":"comment3"},{"atom":"pi4"},{"text":"Z"}])
        );
    }

    #[test]
    fn s09_special_spaces_never_enter_text_parts() {
        let (src, out) = extract_body("<p>A&#160;B\u{2007}C&#x202F;D</p>");
        assert_eq!(
            parts(&out.segments[0]),
            json!([{"text":"A"},{"atom":"nbsp1"},{"text":"B"},{"atom":"space2"},{"text":"C"},{"atom":"space3"},{"text":"D"}])
        );
        let Token::Atom { range, .. } = out.segments[0].markers[0].token else {
            panic!()
        };
        assert_eq!(&src[range.0..range.1], "&#160;");
    }

    #[test]
    fn s10_empty_anchor_retains_its_entire_lexical_span() {
        let (src, out) = extract_body("<p>Intro <a id='chapter'></a> continues.</p>");
        assert_eq!(out.segments[0].markers[0].id, "a1");
        let Token::Atom { range, .. } = out.segments[0].markers[0].token else {
            panic!()
        };
        assert_eq!(&src[range.0..range.1], "<a id='chapter'></a>");
    }

    #[test]
    fn s11_atom_only_and_empty_content_remains_preserved() {
        let (_, out) = extract_body(
            "<p> \n <i/> <br/></p><div><svg xmlns='http://www.w3.org/2000/svg'><image/></svg></div><style>x</style><p/>",
        );
        assert!(out.segments.is_empty());
        assert_eq!(
            out.preserved.iter().map(|p| p.reason).collect::<Vec<_>>(),
            ["atom", "atom", "svg", "style"]
        );
    }

    #[test]
    fn s12_unknown_and_structurally_incompatible_content_fails() {
        for body in [
            "<p>A<ruby>unknown</ruby></p>",
            "<table>lost</table>",
            "<p><span><div>block</div></span></p>",
            "<pre>preserve</pre>",
            "<p><img>lost</img></p>",
            "<p><x:i xmlns:x='urn:other'>text</x:i></p>",
        ] {
            let source = format!(
                "<html xmlns='{}'><body>{body}</body></html>",
                xhtml::XHTML_NS
            );
            let doc = xhtml::parse(&source, "body.xhtml").unwrap();
            assert!(
                matches!(
                    extract(&doc, "body.xhtml"),
                    Err(MinamiError::Unsupported { .. })
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn s13_coverage_cdata_merge_and_marker_counter_reset() {
        let (_, out) = extract_body("<div><p>A<![CDATA[B]]>C<i>D</i>E</p><p><b>F</b></p></div>");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(
            parts(&out.segments[0]),
            json!([{"text":"ABC"},{"open":"i1"},{"text":"D"},{"close":"i1"},{"text":"E"}])
        );
        assert_eq!(out.segments[1].markers[0].id, "b1");
        assert!(out.segments[0].range.1 <= out.segments[1].range.0);
    }

    #[test]
    fn p01_structural_comment_has_one_exact_preserved_span() {
        let (source, out) = extract_body("<ul><!--important--><li>Text</li></ul>");
        assert_eq!(out.segments.len(), 1);
        assert_eq!(parts(&out.segments[0]), json!([{"text":"Text"}]));
        assert_eq!(out.preserved.len(), 1);
        let preserved = &out.preserved[0];
        assert_eq!(preserved.reason, "atom");
        assert_eq!(
            &source[preserved.range.0..preserved.range.1],
            "<!--important-->"
        );
        assert!(preserved.range.1 <= out.segments[0].range.0);
    }

    #[test]
    fn p02_structural_pi_has_one_exact_preserved_span() {
        let (source, out) = extract_body("<ul><?keep value?><li>Text</li></ul>");
        assert_eq!(out.segments.len(), 1);
        assert_eq!(parts(&out.segments[0]), json!([{"text":"Text"}]));
        assert_eq!(out.preserved.len(), 1);
        let preserved = &out.preserved[0];
        assert_eq!(preserved.reason, "atom");
        assert_eq!(
            &source[preserved.range.0..preserved.range.1],
            "<?keep value?>"
        );
        assert!(preserved.range.1 <= out.segments[0].range.0);
    }

    #[test]
    fn p03_structural_atoms_follow_source_order_without_duplicates() {
        let (source, out) = extract_body(
            "<ul>\n  <!--first-->\n  <?keep value?>\n  <li>Text</li>\n  <!--last-->\n</ul>",
        );
        assert_eq!(out.segments.len(), 1);
        assert_eq!(parts(&out.segments[0]), json!([{"text":"Text"}]));
        let raw: Vec<_> = out
            .preserved
            .iter()
            .map(|p| {
                assert_eq!(p.reason, "atom");
                assert!(
                    out.segments
                        .iter()
                        .all(|s| p.range.1 <= s.range.0 || s.range.1 <= p.range.0)
                );
                &source[p.range.0..p.range.1]
            })
            .collect();
        assert_eq!(raw, ["<!--first-->", "<?keep value?>", "<!--last-->"]);
        assert!(
            out.preserved
                .windows(2)
                .all(|pair| pair[0].range.1 <= pair[1].range.0)
        );
    }

    #[test]
    fn p04_comment_and_pi_inside_segments_are_not_preserved_twice() {
        for (raw, id) in [("<!--keep-->", "comment1"), ("<?keep value?>", "pi1")] {
            let (source, out) = extract_body(&format!("<p>Before{raw}After</p>"));
            assert_eq!(out.segments.len(), 1);
            assert_eq!(
                parts(&out.segments[0]),
                json!([{"text":"Before"},{"atom":id},{"text":"After"}])
            );
            assert!(out.preserved.is_empty());
            assert_eq!(out.segments[0].markers.len(), 1);
            let Token::Atom { range, .. } = out.segments[0].markers[0].token else {
                panic!("comment/PI must remain one atom");
            };
            assert_eq!(&source[range.0..range.1], raw);
        }
    }

    #[test]
    fn p05_structural_whitespace_is_preserved_implicitly() {
        let (_, out) = extract_body("<ul>\n  <li>First</li>\n  <li>Second</li>\n</ul>");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(parts(&out.segments[0]), json!([{"text":"First"}]));
        assert_eq!(parts(&out.segments[1]), json!([{"text":"Second"}]));
        assert!(out.preserved.is_empty());
        assert!(out.segments.iter().all(|s| s.markers.is_empty()));
        assert!(out.segments[0].range.1 <= out.segments[1].range.0);
    }

    #[test]
    fn x17_source_atom_order_and_distinct_parents_are_recoverable() {
        let (_, out) = extract_body("<p><i>One<br/>two</i><img src=\"image.png\"/> End.</p>");
        let draft = &out.segments[0];
        assert_eq!(
            draft
                .markers
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["i1", "br2", "img3"]
        );
        assert!(matches!(&draft.markers[1].token,Token::Atom{parent:Some(p),..} if p=="i1"));
        assert!(matches!(
            &draft.markers[2].token,
            Token::Atom { parent: None, .. }
        ));
        assert_eq!(
            draft
                .parts
                .iter()
                .filter_map(|p| if let Part::Atom(id) = p {
                    Some(id.as_str())
                } else {
                    None
                })
                .collect::<Vec<_>>(),
            ["br2", "img3"]
        );
    }

    #[test]
    fn paired_marker_nesting_has_its_own_budget() {
        let (_, allowed) = extract_body(&format!(
            "<p>{}Text{}</p>",
            "<i>".repeat(64),
            "</i>".repeat(64)
        ));
        assert_eq!(allowed.segments[0].markers.len(), 64);
        let source = format!(
            "<html xmlns='{}'><body><p>{}Text{}</p></body></html>",
            xhtml::XHTML_NS,
            "<i>".repeat(65),
            "</i>".repeat(65)
        );
        let doc = xhtml::parse(&source, "deep.xhtml").unwrap();
        assert!(matches!(
            extract(&doc, "deep.xhtml"),
            Err(MinamiError::LimitExceeded { .. })
        ));
    }
}
