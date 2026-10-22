use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ManifestResource, MinamiError, NavigationEntry,
    epub::xhtml::{self, ByteSpan, Document, Kind, XHTML_NS},
    x,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Flow,
    Structural,
    Inline,
    Atom,
    Boundary,
}

struct Expected {
    parts: Vec<x::Part>,
    markers: Vec<ExpectedMarker>,
}

struct ExpectedMarker {
    id: String,
    parent: Option<String>,
    spans: MarkerSpans,
}

enum MarkerSpans {
    Pair(ByteSpan, ByteSpan),
    Atom(ByteSpan),
}

fn invalid(path: &str, detail: impl Into<String>) -> MinamiError {
    crate::epub::invalid(path, format!("X coverage audit: {}", detail.into()))
}

struct Index {
    subtree_has_link: Vec<bool>,
    sibling_position: Vec<usize>,
}

impl Index {
    fn new(doc: &Document) -> Self {
        let mut subtree_has_link = vec![false; doc.nodes.len()];
        // Nodes are stored in preorder, so reverse order visits children before parents.
        for index in (0..doc.nodes.len()).rev() {
            let node = &doc.nodes[index];
            subtree_has_link[index] = node
                .element()
                .is_some_and(|e| e.namespace == XHTML_NS && e.name == "a" && e.href)
                || node.children.iter().any(|child| subtree_has_link[*child]);
        }
        let mut sibling_position = vec![0; doc.nodes.len()];
        for node in &doc.nodes {
            for (position, child) in node.children.iter().enumerate() {
                sibling_position[*child] = position;
            }
        }
        Self {
            subtree_has_link,
            sibling_position,
        }
    }

    fn sibling(&self, siblings: &[usize], child: usize) -> Option<usize> {
        let position = *self.sibling_position.get(child)?;
        (siblings.get(position) == Some(&child)).then_some(position)
    }
}

fn role(doc: &Document, index: &Index, node_index: usize, path: &str) -> Result<Role, MinamiError> {
    let node = &doc.nodes[node_index];
    let Some(element) = node.element() else {
        return Ok(Role::Atom);
    };
    if element.name == "svg" && element.namespace == "http://www.w3.org/2000/svg" {
        return Ok(Role::Atom);
    }
    if element.namespace != XHTML_NS {
        return Err(invalid(
            path,
            format!("foreign element at byte {}", node.span.0),
        ));
    }
    Ok(match element.name.as_str() {
        "body" | "div" | "center" | "blockquote" | "p" | "h1" | "h2" | "h3" | "h4" | "h5"
        | "h6" | "li" | "dt" | "dd" | "td" | "th" | "caption" => Role::Flow,
        "ol" | "ul" | "dl" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "colgroup" => {
            Role::Structural
        }
        "i" | "b" | "em" | "strong" | "span" | "a" | "sup" | "sub" | "big" | "small" | "font"
            if !matches!(element.name.as_str(), "sup" | "sub")
                || !index.subtree_has_link[node_index] =>
        {
            Role::Inline
        }
        "sup" | "sub" => Role::Atom,
        "img" | "br" => {
            if !node.children.is_empty() {
                return Err(invalid(
                    path,
                    format!("nonempty {} at byte {}", element.name, node.span.0),
                ));
            }
            Role::Atom
        }
        "hr" | "col" | "script" | "style" => Role::Boundary,
        _ => {
            return Err(invalid(
                path,
                format!("unclassified element at byte {}", node.span.0),
            ));
        }
    })
}

fn text_presence(doc: &Document, index: &Index, path: &str) -> Result<Vec<bool>, MinamiError> {
    let mut present = vec![false; doc.nodes.len()];
    for node_index in (0..doc.nodes.len()).rev() {
        let mut ancestor = Some(node_index);
        let mut inside_body = false;
        while let Some(current) = ancestor {
            if current == doc.body {
                inside_body = true;
                break;
            }
            ancestor = doc.nodes[current].parent;
        }
        if !inside_body {
            continue;
        }
        let mut parent = if node_index == doc.body {
            None
        } else {
            doc.nodes[node_index].parent
        };
        let mut opaque_parent = false;
        while let Some(ancestor) = parent {
            if ancestor == doc.body {
                break;
            }
            if doc.nodes[ancestor]
                .element()
                .is_some_and(|element| element.namespace == "http://www.w3.org/2000/svg")
                || (doc.nodes[ancestor].element().is_some()
                    && matches!(
                        role(doc, index, ancestor, path)?,
                        Role::Atom | Role::Boundary
                    ))
            {
                opaque_parent = true;
                break;
            }
            parent = doc.nodes[ancestor].parent;
        }
        if opaque_parent {
            continue;
        }
        let node = &doc.nodes[node_index];
        present[node_index] = if node.element().is_some() {
            if matches!(
                role(doc, index, node_index, path)?,
                Role::Atom | Role::Boundary
            ) {
                false
            } else {
                node.children.iter().any(|child| present[*child])
            }
        } else if let Kind::Text(chunks) = &node.kind {
            chunks.iter().any(|chunk| {
                chunk
                    .text
                    .chars()
                    .any(|c| !xhtml::xml_space(c) && !xhtml::protected_space(c))
            })
        } else {
            false
        };
    }
    Ok(present)
}

fn text(parts: &mut Vec<x::Part>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(x::Part::Text(last)) = parts.last_mut() {
        last.push_str(value);
    } else {
        parts.push(x::Part::Text(value.to_owned()));
    }
}

fn atom(expected: &mut Expected, name: &str, span: ByteSpan, parent: Option<&str>) {
    let id = format!("{name}{}", expected.markers.len() + 1);
    expected.parts.push(x::Part::Atom(id.clone()));
    expected.markers.push(ExpectedMarker {
        id,
        parent: parent.map(str::to_owned),
        spans: MarkerSpans::Atom(span),
    });
}

#[allow(clippy::too_many_arguments)]
fn emit(
    doc: &Document,
    index: &Index,
    text_present: &[bool],
    node_index: usize,
    parent: Option<&str>,
    span: ByteSpan,
    expected: &mut Expected,
    path: &str,
) -> Result<(), MinamiError> {
    let node = &doc.nodes[node_index];
    if node.span.1 <= span.0 || span.1 <= node.span.0 {
        return Ok(());
    }
    match &node.kind {
        Kind::Text(chunks) => {
            for chunk in chunks {
                if chunk.span.1 <= span.0 || span.1 <= chunk.span.0 {
                    continue;
                }
                if chunk.span.0 < span.0 || chunk.span.1 > span.1 {
                    return Err(invalid(
                        path,
                        format!("segment splits text/reference at byte {}", chunk.span.0),
                    ));
                }
                if let Some(scalar) = chunk
                    .text
                    .chars()
                    .next()
                    .filter(|c| xhtml::protected_space(*c))
                {
                    atom(
                        expected,
                        if scalar == '\u{a0}' { "nbsp" } else { "space" },
                        chunk.span,
                        parent,
                    );
                } else {
                    text(&mut expected.parts, &chunk.text);
                }
            }
        }
        Kind::Comment | Kind::Pi => {
            if node.span.0 < span.0 || node.span.1 > span.1 {
                return Err(invalid(
                    path,
                    format!("segment splits an XML atom at byte {}", node.span.0),
                ));
            }
            atom(
                expected,
                if matches!(node.kind, Kind::Comment) {
                    "comment"
                } else {
                    "pi"
                },
                node.span,
                parent,
            );
        }
        Kind::Root => return Err(invalid(path, "unexpected lexical root")),
        Kind::Element(element) => match role(doc, index, node_index, path)? {
            Role::Atom => {
                if node.span.0 < span.0 || node.span.1 > span.1 {
                    return Err(invalid(
                        path,
                        format!("segment splits opaque element at byte {}", node.span.0),
                    ));
                }
                atom(expected, &element.name, node.span, parent);
            }
            Role::Boundary => {
                return Err(invalid(
                    path,
                    format!("segment crosses preserved boundary at byte {}", node.span.0),
                ));
            }
            Role::Flow | Role::Structural => {
                for child in children_in_range(doc, node_index, span) {
                    emit(
                        doc,
                        index,
                        text_present,
                        *child,
                        parent,
                        span,
                        expected,
                        path,
                    )?;
                }
            }
            Role::Inline => {
                if !text_present[node_index] {
                    if node.span.0 < span.0 || node.span.1 > span.1 {
                        return Err(invalid(
                            path,
                            format!("segment splits empty inline at byte {}", node.span.0),
                        ));
                    }
                    atom(expected, &element.name, node.span, parent);
                } else {
                    let close = element
                        .close
                        .ok_or_else(|| invalid(path, "paired marker has no close tag"))?;
                    if node.span.0 < span.0 || node.span.1 > span.1 {
                        return Err(invalid(
                            path,
                            format!("segment splits inline element at byte {}", node.span.0),
                        ));
                    }
                    let id = format!("{}{}", element.name, expected.markers.len() + 1);
                    expected.markers.push(ExpectedMarker {
                        id: id.clone(),
                        parent: parent.map(str::to_owned),
                        spans: MarkerSpans::Pair(element.open, close),
                    });
                    expected.parts.push(x::Part::Open(id.clone()));
                    for child in children_in_range(doc, node_index, span) {
                        emit(
                            doc,
                            index,
                            text_present,
                            *child,
                            Some(&id),
                            span,
                            expected,
                            path,
                        )?;
                    }
                    expected.parts.push(x::Part::Close(id));
                }
            }
        },
    }
    Ok(())
}

fn children_in_range(doc: &Document, index: usize, span: ByteSpan) -> &[usize] {
    let children = &doc.nodes[index].children;
    let start = children.partition_point(|child| doc.nodes[*child].span.1 <= span.0);
    let end = start + children[start..].partition_point(|child| doc.nodes[*child].span.0 < span.1);
    &children[start..end]
}

fn expected_segment(
    doc: &Document,
    index: &Index,
    text_present: &[bool],
    span: ByteSpan,
    path: &str,
) -> Result<Expected, MinamiError> {
    let mut expected = Expected {
        parts: Vec::new(),
        markers: Vec::new(),
    };
    emit(
        doc,
        index,
        text_present,
        doc.body,
        None,
        span,
        &mut expected,
        path,
    )?;
    Ok(expected)
}

fn matches_marker(expected: &ExpectedMarker, actual: &x::Marker) -> bool {
    if expected.id != actual.id {
        return false;
    }
    match (&expected.spans, &actual.token) {
        (
            MarkerSpans::Pair(open, close),
            x::Token::Pair {
                parent,
                open: actual_open,
                close: actual_close,
            },
        ) => parent == &expected.parent && open == actual_open && close == actual_close,
        (MarkerSpans::Atom(span), x::Token::Atom { parent, range }) => {
            parent == &expected.parent && span == range
        }
        _ => false,
    }
}

fn covered(segments: &[x::Segment], span: ByteSpan) -> Option<&x::Segment> {
    let index = segments.partition_point(|segment| segment.range.0 <= span.0);
    index
        .checked_sub(1)
        .and_then(|i| segments.get(i))
        .filter(|segment| span.0 < segment.range.1)
}

fn group_child(
    doc: &Document,
    index: &Index,
    node_index: usize,
    path: &str,
) -> Result<bool, MinamiError> {
    Ok(match &doc.nodes[node_index].kind {
        Kind::Text(_) | Kind::Comment | Kind::Pi => true,
        Kind::Element(_) => matches!(
            role(doc, index, node_index, path)?,
            Role::Inline | Role::Atom
        ),
        Kind::Root => false,
    })
}

fn check_segment_group(
    doc: &Document,
    index: &Index,
    starts: &BTreeMap<usize, usize>,
    ends: &BTreeMap<usize, usize>,
    span: ByteSpan,
    path: &str,
) -> Result<(), MinamiError> {
    let start = starts
        .get(&span.0)
        .copied()
        .ok_or_else(|| invalid(path, "segment start is not a lexical node boundary"))?;
    let end = ends
        .get(&span.1)
        .copied()
        .ok_or_else(|| invalid(path, "segment end is not a lexical node boundary"))?;
    let group_parent = |mut node: usize| -> Result<(usize, usize), MinamiError> {
        loop {
            let parent = doc.nodes[node]
                .parent
                .ok_or_else(|| invalid(path, "segment has no flow parent"))?;
            if parent == doc.body || role(doc, index, parent, path)? == Role::Flow {
                return Ok((parent, node));
            }
            node = parent;
        }
    };
    let (parent, first_child) = group_parent(start)?;
    let siblings = &doc.nodes[parent].children;
    let first = index
        .sibling(siblings, first_child)
        .ok_or_else(|| invalid(path, "segment start parent is inconsistent"))?;
    if first > 0 && group_child(doc, index, siblings[first - 1], path)? {
        return Err(invalid(
            path,
            format!(
                "segment starting at byte {} omits the beginning of its flow group",
                span.0
            ),
        ));
    }
    let (end_parent, last_child) = group_parent(end)?;
    if end_parent != parent {
        return Err(invalid(path, "segment crosses flow groups"));
    }
    let last = index
        .sibling(siblings, last_child)
        .ok_or_else(|| invalid(path, "segment end parent is inconsistent"))?;
    if last + 1 < siblings.len() && group_child(doc, index, siblings[last + 1], path)? {
        return Err(invalid(
            path,
            format!(
                "segment ending at byte {} omits the end of its flow group",
                span.1
            ),
        ));
    }
    Ok(())
}

fn whitespace_node(doc: &Document, index: usize) -> bool {
    match &doc.nodes[index].kind {
        Kind::Text(chunks) => chunks
            .iter()
            .all(|chunk| chunk.text.chars().all(xhtml::xml_space)),
        _ => false,
    }
}

fn before_anchor(
    doc: &Document,
    index: &Index,
    node_index: usize,
    target: usize,
    path: &str,
) -> Result<bool, MinamiError> {
    if node_index == target {
        return Ok(true);
    }
    let node = &doc.nodes[node_index];
    match &node.kind {
        Kind::Text(_) => Ok(whitespace_node(doc, node_index)),
        Kind::Comment | Kind::Pi | Kind::Root => Ok(false),
        Kind::Element(_) => {
            if role(doc, index, node_index, path)? != Role::Inline
                || doc.nodes[target].span.0 <= node.span.0
                || doc.nodes[target].span.1 > node.span.1
            {
                return Ok(false);
            }
            for child in &node.children {
                if doc.nodes[*child].span.0 <= doc.nodes[target].span.0 {
                    if !before_anchor(doc, index, *child, target, path)? {
                        return Ok(false);
                    }
                    if *child == target
                        || (doc.nodes[target].span.0 < doc.nodes[*child].span.1
                            && doc.nodes[target].span.1 <= doc.nodes[*child].span.1)
                    {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
    }
}

fn normalized_anchor(
    doc: &Document,
    audit: &Index,
    target: usize,
    body: ByteSpan,
    path: &str,
) -> Result<usize, MinamiError> {
    match role(doc, audit, target, path)? {
        Role::Flow | Role::Structural => {
            return Ok(if target == doc.body {
                body.0
            } else {
                doc.nodes[target].span.0
            });
        }
        Role::Atom | Role::Boundary => {
            return Err(invalid(
                path,
                format!("NCX target at byte {} is opaque", doc.nodes[target].span.0),
            ));
        }
        Role::Inline => {}
    }
    let mut parent = doc.nodes[target].parent;
    let mut direct = target;
    while let Some(index) = parent {
        if index == doc.body
            || doc.nodes[index]
                .element()
                .is_some_and(|_| role(doc, audit, index, path).is_ok_and(|r| r == Role::Flow))
        {
            break;
        }
        match &doc.nodes[index].kind {
            Kind::Element(_) if matches!(role(doc, audit, index, path)?, Role::Inline) => {}
            _ => {
                return Err(invalid(
                    path,
                    format!(
                        "NCX target at byte {} is inside opaque content",
                        doc.nodes[target].span.0
                    ),
                ));
            }
        }
        direct = index;
        parent = doc.nodes[index].parent;
    }
    let parent = parent.ok_or_else(|| invalid(path, "inline NCX target has no flow container"))?;
    let siblings = &doc.nodes[parent].children;
    let target_pos = audit
        .sibling(siblings, direct)
        .ok_or_else(|| invalid(path, "NCX target parent mapping is inconsistent"))?;
    let mut first = target_pos;
    while first > 0 {
        let sibling = siblings[first - 1];
        let allowed = match &doc.nodes[sibling].kind {
            Kind::Text(_) | Kind::Comment | Kind::Pi => true,
            Kind::Element(_) => role(doc, audit, sibling, path)
                .is_ok_and(|r| matches!(r, Role::Inline | Role::Atom)),
            Kind::Root => false,
        };
        if !allowed {
            break;
        }
        first -= 1;
    }
    for sibling in &siblings[first..target_pos] {
        if !whitespace_node(doc, *sibling) {
            return Err(invalid(
                path,
                format!(
                    "NCX inline target at byte {} follows content in its flow group",
                    doc.nodes[target].span.0
                ),
            ));
        }
    }
    if !before_anchor(doc, audit, direct, target, path)? {
        return Err(invalid(
            path,
            format!(
                "NCX inline target at byte {} follows content in its flow group",
                doc.nodes[target].span.0
            ),
        ));
    }
    Ok(doc.nodes[siblings[first]].span.0)
}

fn navigation_boundary(
    doc: &Document,
    index: &Index,
    entry: &NavigationEntry,
    body: ByteSpan,
    path: &str,
) -> Result<usize, MinamiError> {
    match entry.fragment.as_deref() {
        None => Ok(body.0),
        Some("") => Err(invalid(path, "empty NCX fragment")),
        Some(fragment) => {
            let target = *doc.ids.get(fragment).ok_or_else(|| {
                invalid(
                    path,
                    format!("NCX fragment #{fragment} is absent from XHTML"),
                )
            })?;
            normalized_anchor(doc, index, target, body, path)
        }
    }
}

fn eligible_text(
    doc: &Document,
    index: &Index,
    mut node: usize,
    path: &str,
) -> Result<bool, MinamiError> {
    while let Some(parent) = doc.nodes[node].parent {
        if parent == doc.body {
            return Ok(true);
        }
        if doc.nodes[parent]
            .element()
            .is_some_and(|element| element.namespace == "http://www.w3.org/2000/svg")
        {
            return Ok(false);
        }
        if doc.nodes[parent].element().is_some()
            && matches!(role(doc, index, parent, path)?, Role::Atom | Role::Boundary)
        {
            return Ok(false);
        }
        node = parent;
    }
    Ok(false)
}

fn preserved_obligations(
    doc: &Document,
    index: &Index,
    text_present: &[bool],
    node_index: usize,
    segments: &[x::Segment],
    expected: &mut Vec<(ByteSpan, &'static str)>,
    path: &str,
) -> Result<(), MinamiError> {
    let node = &doc.nodes[node_index];
    match &node.kind {
        Kind::Text(chunks) => {
            for chunk in chunks {
                if chunk
                    .text
                    .chars()
                    .next()
                    .is_some_and(xhtml::protected_space)
                    && covered(segments, chunk.span).is_none()
                {
                    expected.push((chunk.span, "atom"));
                }
            }
        }
        Kind::Comment | Kind::Pi => {
            if covered(segments, node.span).is_none() {
                expected.push((node.span, "atom"));
            }
        }
        Kind::Element(element) => match role(doc, index, node_index, path)? {
            Role::Boundary if matches!(element.name.as_str(), "script" | "style") => {
                expected.push((
                    node.span,
                    if element.name == "script" {
                        "script"
                    } else {
                        "style"
                    },
                ))
            }
            Role::Atom if covered(segments, node.span).is_none() => expected.push((
                node.span,
                if element.namespace == "http://www.w3.org/2000/svg" {
                    "svg"
                } else {
                    "atom"
                },
            )),
            Role::Inline if !text_present[node_index] && covered(segments, node.span).is_none() => {
                expected.push((node.span, "atom"))
            }
            Role::Atom | Role::Boundary => return Ok(()),
            _ => {
                for child in &node.children {
                    preserved_obligations(
                        doc,
                        index,
                        text_present,
                        *child,
                        segments,
                        expected,
                        path,
                    )?;
                }
            }
        },
        Kind::Root => return Err(invalid(path, "unexpected root in body")),
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct ValidationFacts {
    pub segments: BTreeMap<String, SegmentFacts>,
}

#[derive(Debug, Default)]
pub(crate) struct SegmentFacts {
    pub anchors: BTreeSet<String>,
    pub promoted: Vec<PromotedMarker>,
}

#[derive(Debug)]
pub(crate) struct PromotedMarker {
    pub id: String,
    pub ancestors: BTreeSet<String>,
}

pub(crate) fn validation_facts(
    doc: &Document,
    path: &str,
    segments: &[x::Segment],
    navigation: &[NavigationEntry],
) -> Result<ValidationFacts, MinamiError> {
    let index = Index::new(doc);
    let mut element_by_open = BTreeMap::new();
    let mut id_elements = Vec::new();
    for (node_index, node) in doc.nodes.iter().enumerate() {
        let Some(element) = node.element() else {
            continue;
        };
        element_by_open.insert(element.open.0, node_index);
        if element.id.is_some() {
            id_elements.push((node.span.0, node.span.1, node_index));
        }
    }
    id_elements.sort_by_key(|(start, _, _)| *start);

    let mut built = Vec::with_capacity(segments.len());
    for segment in segments {
        let mut facts = SegmentFacts::default();
        let mut parents = BTreeMap::new();
        let mut node_marker = BTreeMap::new();
        for marker in &segment.markers {
            let parent = match &marker.token {
                x::Token::Pair { parent, open, .. } => {
                    let node = *element_by_open.get(&open.0).ok_or_else(|| {
                        invalid(
                            path,
                            format!("marker {} does not match an original element", marker.id),
                        )
                    })?;
                    if doc.nodes[node]
                        .element()
                        .is_some_and(|element| element.id.is_some())
                    {
                        facts.anchors.insert(marker.id.clone());
                    }
                    if node_marker.insert(node, marker.id.as_str()).is_some() {
                        return Err(invalid(
                            path,
                            format!("marker {} duplicates an original element", marker.id),
                        ));
                    }
                    parent
                }
                x::Token::Atom { parent, range } => {
                    let first = id_elements.partition_point(|(start, _, _)| *start < range.0);
                    for (start, end, node) in &id_elements[first..] {
                        if *start >= range.1 {
                            break;
                        }
                        if *end <= range.1 {
                            facts.anchors.insert(marker.id.clone());
                            if node_marker.insert(*node, marker.id.as_str()).is_some() {
                                return Err(invalid(
                                    path,
                                    format!("marker {} duplicates an original element", marker.id),
                                ));
                            }
                        }
                    }
                    parent
                }
            };
            parents.insert(marker.id.as_str(), parent.as_deref());
        }
        built.push((facts, parents, node_marker));
    }

    for entry in navigation.iter().filter(|entry| entry.path == path) {
        let Some(fragment) = entry.fragment.as_deref() else {
            continue;
        };
        let target = *doc.ids.get(fragment).ok_or_else(|| {
            invalid(
                path,
                format!("NCX fragment #{fragment} is absent from XHTML"),
            )
        })?;
        if role(doc, &index, target, path)? != Role::Inline {
            continue;
        }
        let start = doc.nodes[target].span.0;
        let Some(position) = segments
            .partition_point(|segment| segment.range.0 <= start)
            .checked_sub(1)
        else {
            continue;
        };
        if start >= segments[position].range.1
            || doc.nodes[target].span.1 > segments[position].range.1
        {
            continue;
        }
        let (facts, parents, node_marker) = &mut built[position];
        let Some(marker) = node_marker.get(&target).copied() else {
            return Err(invalid(
                path,
                format!("NCX target #{fragment} does not map to a marker"),
            ));
        };
        let mut ancestors = BTreeSet::new();
        let mut parent = *parents.get(marker).unwrap_or(&None);
        while let Some(id) = parent {
            if !ancestors.insert(id.to_owned()) {
                break;
            }
            parent = *parents.get(id).unwrap_or(&None);
        }
        if !facts.promoted.iter().any(|promoted| promoted.id == marker) {
            facts.promoted.push(PromotedMarker {
                id: marker.to_owned(),
                ancestors,
            });
        }
    }

    let mut result = ValidationFacts::default();
    for (segment, (facts, _, _)) in segments.iter().zip(built) {
        result.segments.insert(segment.id.clone(), facts);
    }
    Ok(result)
}

pub(crate) struct Records<'a> {
    pub document: &'a x::Document,
    pub sections: &'a [x::Section],
    pub segments: &'a [x::Segment],
    pub source: &'a [x::SourceSegment],
    pub preserved: &'a [x::Preserved],
    pub navigation: &'a [NavigationEntry],
}

pub(crate) fn document(
    doc: &Document,
    path: &str,
    records: Records<'_>,
) -> Result<(), MinamiError> {
    let Records {
        document: mapped_document,
        sections,
        segments,
        source,
        preserved,
        navigation,
    } = records;
    let body = doc.nodes[doc.body]
        .element()
        .ok_or_else(|| invalid(path, "body node missing"))?;
    let expected_body = ByteSpan(body.open.1, body.close.map_or(body.open.1, |span| span.0));
    let index = Index::new(doc);
    let text_present = text_presence(doc, &index, path)?;
    let starts: BTreeMap<_, _> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.span.0, index))
        .collect();
    let ends: BTreeMap<_, _> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.span.1, index))
        .collect();
    if mapped_document.body != expected_body {
        return Err(invalid(path, "body span differs from the original XHTML"));
    }
    if sections.is_empty() {
        return Err(invalid(path, "document has no section"));
    }
    let mut end = expected_body.0;
    for section in sections {
        if section.document_id != mapped_document.id
            || section.range.0 != end
            || section.range.1 < end
            || section.range.1 > expected_body.1
        {
            return Err(invalid(
                path,
                format!("section {} does not partition the body", section.id),
            ));
        }
        end = section.range.1;
    }
    if end != expected_body.1 {
        return Err(invalid(path, "sections do not cover the body end"));
    }
    let mut boundaries =
        std::collections::BTreeMap::<usize, Vec<usize>>::from([(expected_body.0, Vec::new())]);
    for entry in navigation.iter().filter(|entry| entry.path == path) {
        let boundary = navigation_boundary(doc, &index, entry, expected_body, path)?;
        if boundary < expected_body.0 || boundary > expected_body.1 {
            return Err(invalid(path, "NCX target falls outside body"));
        }
        boundaries.entry(boundary).or_default().push(entry.position);
    }
    let expected_sections: Vec<_> = boundaries.into_iter().collect();
    if expected_sections.len() != sections.len() {
        return Err(invalid(
            path,
            "sections do not match normalized NCX destinations",
        ));
    }
    for (index, (section, (start, positions))) in
        sections.iter().zip(&expected_sections).enumerate()
    {
        let expected_end = expected_sections
            .get(index + 1)
            .map_or(expected_body.1, |next| next.0);
        if section.range != ByteSpan(*start, expected_end)
            || section.navigation_positions != *positions
        {
            return Err(invalid(
                path,
                format!("section {} does not match NCX/body boundaries", section.id),
            ));
        }
    }
    if segments.len() != source.len() {
        return Err(invalid(path, "control/source segment counts differ"));
    }
    let sections_by_id: BTreeMap<_, _> = sections
        .iter()
        .map(|section| (section.id.as_str(), section))
        .collect();
    let mut previous_end = expected_body.0;
    let mut seen = BTreeSet::new();
    for (segment, source) in segments.iter().zip(source) {
        if segment.document_id != mapped_document.id || !seen.insert(&segment.id) {
            return Err(invalid(
                path,
                format!("duplicate or foreign segment {}", segment.id),
            ));
        }
        if source.id != segment.id {
            return Err(invalid(
                path,
                format!("source segment does not match {}", segment.id),
            ));
        }
        if segment.range.0 < previous_end
            || segment.range.0 >= segment.range.1
            || segment.range.1 > expected_body.1
        {
            return Err(invalid(
                path,
                format!("invalid or overlapping segment {}", segment.id),
            ));
        }
        check_segment_group(doc, &index, &starts, &ends, segment.range, path)?;
        let section = sections_by_id
            .get(segment.section_id.as_str())
            .copied()
            .ok_or_else(|| {
                invalid(
                    path,
                    format!("segment {} references missing section", segment.id),
                )
            })?;
        if segment.range.0 < section.range.0 || segment.range.1 > section.range.1 {
            return Err(invalid(
                path,
                format!("segment {} crosses section {}", segment.id, section.id),
            ));
        }
        let expected = expected_segment(doc, &index, &text_present, segment.range, path)?;
        if expected.parts != source.parts {
            return Err(invalid(
                path,
                format!(
                    "segment {} text/marker sequence differs from its XHTML source",
                    segment.id
                ),
            ));
        }
        if expected.parts.iter().all(|part| !matches!(part, x::Part::Text(text) if text.chars().any(|c| !xhtml::xml_space(c) && !xhtml::protected_space(c)))) {
            return Err(invalid(path, format!("segment {} has no eligible text", segment.id)));
        }
        if expected.markers.len() != segment.markers.len()
            || !expected
                .markers
                .iter()
                .zip(&segment.markers)
                .all(|(a, b)| matches_marker(a, b))
        {
            return Err(invalid(
                path,
                format!(
                    "segment {} markers differ from original tags/atoms",
                    segment.id
                ),
            ));
        }
        previous_end = segment.range.1;
    }
    let mut obligations = Vec::new();
    preserved_obligations(
        doc,
        &index,
        &text_present,
        doc.body,
        segments,
        &mut obligations,
        path,
    )?;
    obligations.sort_by_key(|(span, _)| span.0);
    if obligations.len() != preserved.len() {
        return Err(invalid(
            path,
            "preserved record count differs from XHTML obligations",
        ));
    }
    let mut last = expected_body.0;
    let mut seen = BTreeSet::new();
    for ((span, reason), record) in obligations.iter().zip(preserved) {
        if record.document_id != mapped_document.id
            || record.range != *span
            || record.reason != *reason
            || span.0 < last
            || !seen.insert((span.0, span.1))
        {
            return Err(invalid(
                path,
                format!(
                    "invalid, duplicate or out-of-order preserved record at byte {}",
                    record.range.0
                ),
            ));
        }
        last = span.1;
    }
    // Every eligible text chunk must be wholly represented by one segment.
    for (node_index, node) in doc.nodes.iter().enumerate().skip(doc.body) {
        if node.span.0 < expected_body.0 || node.span.1 > expected_body.1 {
            continue;
        }
        if let Kind::Text(chunks) = &node.kind {
            if !eligible_text(doc, &index, node_index, path)? {
                continue;
            }
            for chunk in chunks {
                if chunk
                    .text
                    .chars()
                    .any(|c| !xhtml::xml_space(c) && !xhtml::protected_space(c))
                {
                    let Some(segment) = covered(segments, chunk.span) else {
                        return Err(invalid(
                            path,
                            format!("eligible text omitted at byte {}", chunk.span.0),
                        ));
                    };
                    if chunk.span.0 < segment.range.0 || chunk.span.1 > segment.range.1 {
                        return Err(invalid(
                            path,
                            format!("text occurrence split at byte {}", chunk.span.0),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn package(
    resources: &[ManifestResource],
    spine: &[crate::SpineEntry],
    control: &x::Control,
    path: &str,
) -> Result<(), MinamiError> {
    if control.spine != spine {
        return Err(invalid(path, "spine differs from the original package"));
    }
    let by_id: std::collections::BTreeMap<_, _> = resources
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in &control.spine {
        let index = *by_id
            .get(entry.idref.as_str())
            .ok_or_else(|| invalid(path, "spine references missing manifest item"))?;
        if resources[index].media_type != crate::epub::XHTML {
            return Err(invalid(
                path,
                format!("spine item {} is not XHTML", entry.idref),
            ));
        }
        if seen.insert(index) {
            order.push(index);
        }
    }
    for (index, resource) in resources.iter().enumerate() {
        if resource.media_type == crate::epub::XHTML && seen.insert(index) {
            order.push(index);
        }
    }
    if control.documents.len() != order.len() {
        return Err(invalid(
            path,
            "manifest XHTML document coverage is incomplete",
        ));
    }
    for (i, (index, document)) in order.iter().zip(&control.documents).enumerate() {
        let resource = &resources[*index];
        if document.id != format!("d{}", i + 1)
            || document.manifest_id != resource.id
            || document.path != resource.path
        {
            return Err(invalid(
                path,
                format!(
                    "document {} does not match manifest order/resource",
                    document.id
                ),
            ));
        }
    }
    let document_order: std::collections::BTreeMap<_, _> = control
        .documents
        .iter()
        .enumerate()
        .map(|(index, document)| (document.id.as_str(), index))
        .collect();
    if document_order.len() != control.documents.len() {
        return Err(invalid(path, "duplicate document ID"));
    }
    let mut previous = 0;
    for (index, section) in control.sections.iter().enumerate() {
        let order = *document_order
            .get(section.document_id.as_str())
            .ok_or_else(|| invalid(path, "section references unknown document"))?;
        if order < previous || section.id != format!("q{}", index + 1) {
            return Err(invalid(path, "section IDs/document order are inconsistent"));
        }
        previous = order;
    }
    previous = 0;
    for (index, segment) in control.segments.iter().enumerate() {
        let order = *document_order
            .get(segment.document_id.as_str())
            .ok_or_else(|| invalid(path, "segment references unknown document"))?;
        if order < previous || segment.id != format!("s{}", index + 1) {
            return Err(invalid(path, "segment IDs/document order are inconsistent"));
        }
        previous = order;
    }
    if control
        .preserved
        .iter()
        .any(|record| !document_order.contains_key(record.document_id.as_str()))
    {
        return Err(invalid(
            path,
            "preserved record references unknown document",
        ));
    }
    let mut document = 0;
    let mut end = 0;
    for record in &control.preserved {
        let order = document_order[record.document_id.as_str()];
        if order < document || (order == document && record.range.0 < end) {
            return Err(invalid(
                path,
                format!(
                    "preserved record at byte {} is out of document/source order",
                    record.range.0
                ),
            ));
        }
        document = order;
        end = record.range.1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{
            extract::{self, Extraction},
            support::*,
        },
        epub::{archive, xhtml},
    };

    fn valid(body: &str) -> (xhtml::Document, Extraction) {
        valid_targets(body, &["Text/Body.xhtml"])
    }

    fn wrap(body: &str) -> String {
        format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
    }

    fn valid_targets(body: &str, targets: &[&str]) -> (xhtml::Document, Extraction) {
        let mut entries = entries();
        replace(&mut entries, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(&mut entries, "OEBPS/Text/Illustrations.xhtml", wrap(""));
        replace(&mut entries, "OEBPS/Text/Body.xhtml", wrap(body));
        let points = targets.iter().enumerate().map(|(i, target)| format!(
            "<navPoint id='n{i}'><navLabel><text>Section</text></navLabel><content src='{target}'/></navPoint>"
        )).collect::<String>();
        replace(
            &mut entries,
            "OEBPS/toc.ncx",
            format!(
                "<ncx xmlns='http://www.daisy.org/z3986/2005/ncx/' version='2005-1'><navMap>{points}</navMap></ncx>"
            ),
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.epub");
        std::fs::write(&path, zip_bytes(entries)).unwrap();
        let data = extract::extract_data(&path).unwrap();
        let mut archive = archive::open(&path).unwrap();
        let source = archive.read_xhtml("OEBPS/Text/Body.xhtml").unwrap();
        (
            xhtml::parse(&source, "OEBPS/Text/Body.xhtml").unwrap(),
            data,
        )
    }

    fn check(doc: &xhtml::Document, data: &Extraction) -> Result<(), MinamiError> {
        document(
            doc,
            "OEBPS/Text/Body.xhtml",
            Records {
                document: &data.control.documents[2],
                sections: &data.control.sections[2..],
                segments: &data.control.segments,
                source: &data.source.segments,
                preserved: &data.control.preserved,
                navigation: &data.control.navigation,
            },
        )
    }

    #[test]
    fn audit_accepts_lexical_entities_inline_cdata_crlf_and_protected_spaces() {
        let (doc, data) = valid(
            "<div> Before\r\n<p>Caf&#233; <i>formatted</i>&#160;☕</p>After<![CDATA[!]]></div>",
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn audit_resolves_section_ids_after_the_ninth_section() {
        let body = (0..12)
            .map(|index| format!("<h1 id='h{index}'>Section {index}</h1><p>Text {index}</p>"))
            .collect::<String>();
        let targets = (0..12)
            .map(|index| format!("Text/Body.xhtml#h{index}"))
            .collect::<Vec<_>>();
        let target_refs = targets.iter().map(String::as_str).collect::<Vec<_>>();
        let (doc, data) = valid_targets(&body, &target_refs);
        assert!(data.control.sections.len() >= 12);
        check(&doc, &data).unwrap();
    }

    #[test]
    fn a04_auditor_rejects_text_changed_in_the_mapping() {
        let (doc, mut data) = valid("<p>Original text.</p>");
        let part = data
            .source
            .segments
            .iter_mut()
            .find(|s| s.id == "s1")
            .unwrap()
            .parts
            .first_mut()
            .unwrap();
        *part = x::Part::Text("Changed text.".into());
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a01_auditor_rejects_a_segment_that_drops_group_whitespace() {
        let (doc, mut data) = valid("<p> <i>Words</i> end.</p>");
        data.control.segments[0].range.0 += 1;
        data.source.segments[0].parts.remove(0);
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a02_auditor_rejects_duplicate_nested_text() {
        let (doc, mut data) = valid("<div>Before<p>Inside <i>formatted</i>.</p>After</div>");
        data.source.segments[1]
            .parts
            .push(x::Part::Text("Inside".into()));
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a05_auditor_detects_a_missing_segment_and_text_occurrence() {
        let (doc, mut data) = valid("<p>First.</p><p>Required second.</p>");
        data.control.segments.pop();
        data.source.segments.pop();
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a06_auditor_rejects_a_duplicated_segment() {
        let (doc, mut data) = valid("<p>Only once.</p>");
        let segment = &data.control.segments[0];
        data.control.segments.push(x::Segment {
            id: segment.id.clone(),
            document_id: segment.document_id.clone(),
            section_id: segment.section_id.clone(),
            range: segment.range,
            markers: Vec::new(),
        });
        let source = &data.source.segments[0];
        let duplicate = x::SourceSegment {
            id: source.id.clone(),
            parts: source
                .parts
                .iter()
                .map(|part| match part {
                    x::Part::Text(value) => x::Part::Text(value.clone()),
                    x::Part::Open(value) => x::Part::Open(value.clone()),
                    x::Part::Close(value) => x::Part::Close(value.clone()),
                    x::Part::Atom(value) => x::Part::Atom(value.clone()),
                })
                .collect(),
        };
        data.source.segments.push(duplicate);
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a09_auditor_rejects_text_moved_outside_its_original_inline_marker() {
        let (doc, mut data) = valid("<p>Before <i>inside</i> after.</p>");
        data.source.segments[0].parts = vec![
            x::Part::Text("Before inside".into()),
            x::Part::Open("i1".into()),
            x::Part::Text(" after.".into()),
            x::Part::Close("i1".into()),
        ];
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b05_auditor_rejects_a_marker_span_that_points_at_other_bytes() {
        let (doc, mut data) = valid("<p>Text <i>inside</i>.</p>");
        let x::Token::Pair { open, .. } = &mut data.control.segments[0].markers[0].token else {
            panic!()
        };
        open.0 += 1;
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn c03_c04_auditor_rejects_duplicate_or_wrong_preserved_records() {
        let (doc, mut data) = valid("<ul><!--keep--><li>Text</li></ul>");
        let record = data
            .control
            .preserved
            .iter()
            .find(|p| p.document_id == "d3")
            .unwrap();
        data.control.preserved.push(x::Preserved {
            document_id: record.document_id.clone(),
            range: record.range,
            reason: record.reason,
        });
        assert!(check(&doc, &data).is_err());
        data.control.preserved.pop();
        data.control
            .preserved
            .iter_mut()
            .find(|p| p.document_id == "d3")
            .unwrap()
            .reason = "svg";
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn c05_c06_auditor_keeps_structural_whitespace_implicit() {
        let (doc, data) = valid("<ul>\n <li>First</li>\n <li>Second</li>\n</ul>");
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn d04_d07_auditor_rejects_section_gap_and_wrong_navigation_association() {
        let body = "<h1 id='one'>One</h1><p>A</p><h1 id='two'>Two</h1><p>B</p>";
        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one", "Text/Body.xhtml#two"]);
        assert!(check(&doc, &data).is_ok());
        data.control.sections[2].range.1 -= 1;
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one", "Text/Body.xhtml#two"]);
        data.control.sections[3].navigation_positions.clear();
        assert!(check(&doc, &data).is_err());
        data.control.sections[3].navigation_positions.push(0);
        assert!(check(&doc, &data).is_err());
    }

    fn resources() -> Vec<ManifestResource> {
        vec![
            ManifestResource {
                id: "ncx".into(),
                path: "OEBPS/toc.ncx".into(),
                media_type: crate::epub::NCX.into(),
            },
            ManifestResource {
                id: "body".into(),
                path: "OEBPS/Text/Body.xhtml".into(),
                media_type: crate::epub::XHTML.into(),
            },
            ManifestResource {
                id: "cover".into(),
                path: "OEBPS/Text/Cover.xhtml".into(),
                media_type: crate::epub::XHTML.into(),
            },
            ManifestResource {
                id: "illustrations".into(),
                path: "OEBPS/Text/Illustrations.xhtml".into(),
                media_type: crate::epub::XHTML.into(),
            },
        ]
    }

    fn spine() -> Vec<crate::SpineEntry> {
        vec![
            crate::SpineEntry {
                idref: "cover".into(),
                linear: true,
            },
            crate::SpineEntry {
                idref: "illustrations".into(),
                linear: false,
            },
            crate::SpineEntry {
                idref: "body".into(),
                linear: true,
            },
        ]
    }

    #[test]
    fn d01_auditor_rejects_manifest_coverage_or_resource_mapping_changes() {
        let (_, mut data) = valid("<p>Text</p>");
        let resources = resources();
        let spine = spine();
        assert!(package(&resources, &spine, &data.control, "content.opf").is_ok());
        assert!(package(&resources[0..3], &spine, &data.control, "content.opf").is_err());
        data.control.documents[0].path = "OEBPS/Text/Illustrations.xhtml".into();
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());
    }

    #[test]
    fn a01_direct_text_before_and_after_children_is_never_dropped() {
        let (doc, mut data) = valid("<div>Before<p>Inside</p>After</div>");
        assert_eq!(data.source.segments.len(), 3);
        data.control.segments.remove(2);
        data.source.segments.remove(2);
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid("<div>Before<p>Inside</p>After</div>");
        data.control.segments.remove(0);
        data.source.segments.remove(0);
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a03_inline_text_stays_inside_its_original_marker() {
        let (doc, data) = valid("<p>Before <i>inside</i> after.</p>");
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Before ".into()),
                x::Part::Open("i1".into()),
                x::Part::Text("inside".into()),
                x::Part::Close("i1".into()),
                x::Part::Text(" after.".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn a07_decoded_entities_are_compared_against_original_byte_spans() {
        let body = "<p>Caf&#233; ☕ &amp; tea.</p>";
        let (doc, data) = valid(body);
        assert_eq!(
            data.source.segments[0].parts,
            [x::Part::Text("Café ☕ & tea.".into())]
        );
        let source = wrap(body);
        let range = data.control.segments[0].range;
        assert_eq!(&source[range.0..range.1], "Caf&#233; ☕ &amp; tea.");
        assert!(check(&doc, &data).is_ok());

        let (doc, mut data) = valid(body);
        data.source.segments[0].parts = vec![x::Part::Text("Cafe ☕ & tea.".into())];
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn a08_cdata_and_crlf_use_decoded_text_without_moving_offsets() {
        let body = "<p>A\r\nB<![CDATA[<&nbsp;>]]>C</p>";
        let (doc, data) = valid(body);
        assert_eq!(
            data.source.segments[0].parts,
            [x::Part::Text("A\nB<&nbsp;>C".into())]
        );
        let source = wrap(body);
        let range = data.control.segments[0].range;
        assert_eq!(&source[range.0..range.1], "A\r\nB<![CDATA[<&nbsp;>]]>C");
        assert!(check(&doc, &data).is_ok());

        let (doc, mut data) = valid(body);
        data.source.segments[0].parts = vec![x::Part::Text("A\r\nB<&nbsp;>C".into())];
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b01_b02_b03_segment_bounds_must_be_original_lexical_boundaries() {
        let (doc, mut data) = valid("<p>Text</p>");
        data.control.segments[0].range.1 = doc.nodes[doc.body].span.1 + 1;
        assert!(check(&doc, &data).is_err());

        let body = "<p>Café ☕</p>";
        let (doc, mut data) = valid(body);
        let source = wrap(body);
        let start = data.control.segments[0].range.0;
        data.control.segments[0].range.0 = start + 4;
        assert!(!source.is_char_boundary(start + 4));
        assert!(check(&doc, &data).is_err());

        let body = "<p>A&#233;B</p>";
        let (doc, mut data) = valid(body);
        let source = wrap(body);
        let start = data.control.segments[0].range.0;
        data.control.segments[0].range.0 = start + 2;
        assert_eq!(&source[start + 1..start + 2], "&");
        assert_eq!(&source[start + 2..start + 3], "#");
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b04_overlapping_segments_are_rejected() {
        let (doc, mut data) = valid("<p>One.</p><p>Two.</p>");
        let first = data.control.segments[0].range;
        data.control.segments[1].range.0 = first.0;
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b06_b07_paired_markers_keep_original_tags_and_parents() {
        let (doc, mut data) = valid("<p>Text <i>inside</i>.</p>");
        let x::Token::Pair { close, .. } = &mut data.control.segments[0].markers[0].token else {
            panic!()
        };
        close.0 += 1;
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid("<p><b>Very <i>small</i></b> room.</p>");
        let x::Token::Pair { parent, .. } = &mut data.control.segments[0].markers[1].token else {
            panic!()
        };
        *parent = None;
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b08_atoms_cannot_be_omitted_or_duplicated() {
        let (doc, mut data) = valid("<p>A<br/>B</p>");
        data.source.segments[0].parts.remove(1);
        data.control.segments[0].markers.remove(0);
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid("<p>A<br/>B</p>");
        data.source.segments[0]
            .parts
            .insert(2, x::Part::Atom("br1".into()));
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn b09_b10_svg_and_callout_atoms_never_become_editable_text() {
        let (doc, mut data) =
            valid("<p>A<svg xmlns='http://www.w3.org/2000/svg'><text>Opaque</text></svg>Z</p>");
        data.source.segments[0].parts = vec![
            x::Part::Text("A".into()),
            x::Part::Text("Opaque".into()),
            x::Part::Text("Z".into()),
        ];
        data.control.segments[0].markers.clear();
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid("<p>Word<sup><a href='#note'>[1]</a></sup>.</p>");
        data.source.segments[0].parts = vec![
            x::Part::Text("Word".into()),
            x::Part::Text("[1]".into()),
            x::Part::Text(".".into()),
        ];
        data.control.segments[0].markers.clear();
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn c01_c02_structural_comments_and_processing_instructions_need_records() {
        for body in [
            "<ul><!--keep--><li>Text</li></ul>",
            "<ul><?keep value?><li>Text</li></ul>",
        ] {
            let (doc, mut data) = valid(body);
            assert!(check(&doc, &data).is_ok(), "{body}");
            data.control.preserved.clear();
            assert!(check(&doc, &data).is_err(), "{body}");
        }
    }

    #[test]
    fn c05_content_is_never_counted_as_segment_and_preserved_together() {
        let (doc, mut data) = valid("<p>Before<!--keep-->After</p>");
        let comment = doc
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                Kind::Comment => Some(node.span),
                _ => None,
            })
            .unwrap();
        data.control.preserved.push(x::Preserved {
            document_id: "d3".into(),
            range: comment,
            reason: "atom",
        });
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn c07_protected_spaces_are_atoms_and_never_editable_text() {
        let body = "<p>A&#160;B&#x2007;C</p>";
        let (doc, data) = valid(body);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("A".into()),
                x::Part::Atom("nbsp1".into()),
                x::Part::Text("B".into()),
                x::Part::Atom("space2".into()),
                x::Part::Text("C".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());

        let (doc, mut data) = valid(body);
        data.source.segments[0].parts.remove(1);
        data.control.segments[0].markers.remove(0);
        assert!(check(&doc, &data).is_err());

        let body = "<p>&#160;</p>";
        let (doc, data) = valid(body);
        assert!(data.source.segments.is_empty());
        assert_eq!(data.control.preserved.len(), 1);
        let record = &data.control.preserved[0];
        assert_eq!(record.reason, "atom");
        let source = wrap(body);
        assert_eq!(&source[record.range.0..record.range.1], "&#160;");
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn c08_svg_subtrees_are_opaque_and_preserved_outside_segments() {
        let (doc, data) = valid(
            "<div><svg xmlns='http://www.w3.org/2000/svg'><text>Opaque</text></svg></div><p>Text</p>",
        );
        assert_eq!(data.source.segments.len(), 1);
        assert_eq!(data.control.preserved.len(), 1);
        assert_eq!(data.control.preserved[0].reason, "svg");
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn c09_c10_profile_exclusions_are_not_coverage_failures() {
        for body in ["", "<p> </p>", "<div><br/></div>"] {
            let (doc, data) = valid(body);
            assert!(data.source.segments.is_empty(), "{body}");
            assert!(check(&doc, &data).is_ok(), "{body}");
        }

        let (doc, data) = valid(
            "<p title='untouched'>Text</p><script>var ignored = 1;</script><style>p { color: red }</style>",
        );
        assert_eq!(
            data.source.segments[0].parts,
            [x::Part::Text("Text".into())]
        );
        assert_eq!(
            data.control
                .preserved
                .iter()
                .map(|record| record.reason)
                .collect::<Vec<_>>(),
            ["script", "style"]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn d02_d03_d11_document_coverage_and_order_are_fixed() {
        let resources = resources();
        let spine = spine();
        let (_, mut data) = valid("<p>Text</p>");
        assert!(package(&resources, &spine, &data.control, "content.opf").is_ok());

        data.control.documents.push(x::Document {
            id: "d4".into(),
            manifest_id: "cover".into(),
            path: "OEBPS/Text/Cover.xhtml".into(),
            body: ByteSpan(0, 0),
        });
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());

        let (_, mut data) = valid("<p>Text</p>");
        data.control.documents[2].manifest_id = "cover".into();
        data.control.documents[2].path = "OEBPS/Text/Cover.xhtml".into();
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());

        let (_, mut data) = valid("<p>Text</p>");
        data.control.spine.swap(0, 1);
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());

        let (_, mut data) = valid("<ul><!--one--><li>A</li></ul><ul><!--two--><li>B</li></ul>");
        assert_eq!(data.control.preserved.len(), 2);
        data.control.preserved.swap(0, 1);
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());
    }

    #[test]
    fn d03_manifest_xhtml_outside_the_spine_is_still_covered() {
        let mut book = entries();
        replace(&mut book, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(&mut book, "OEBPS/Text/Illustrations.xhtml", wrap(""));
        replace(
            &mut book,
            "OEBPS/Text/Body.xhtml",
            wrap("<p>Outside spine.</p>"),
        );
        replace(
            &mut book,
            "OEBPS/content.opf",
            OPF.replace("<itemref idref=\"body\"/>", ""),
        );
        replace(
            &mut book,
            "OEBPS/toc.ncx",
            "<ncx xmlns='http://www.daisy.org/z3986/2005/ncx/' version='2005-1'><navMap><navPoint id='n1'><navLabel><text>Body</text></navLabel><content src='Text/Body.xhtml'/></navPoint></navMap></ncx>",
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.epub");
        std::fs::write(&path, zip_bytes(book)).unwrap();
        let mut data = extract::extract_data(&path).unwrap();
        assert_eq!(
            data.control
                .documents
                .iter()
                .map(|document| document.manifest_id.as_str())
                .collect::<Vec<_>>(),
            ["cover", "illustrations", "body"]
        );

        let resources = resources();
        let spine = vec![
            crate::SpineEntry {
                idref: "cover".into(),
                linear: true,
            },
            crate::SpineEntry {
                idref: "illustrations".into(),
                linear: false,
            },
        ];
        assert!(package(&resources, &spine, &data.control, "content.opf").is_ok());
        data.control.documents.pop();
        assert!(package(&resources, &spine, &data.control, "content.opf").is_err());
    }

    #[test]
    fn d04_d05_section_ranges_cannot_overlap_or_leave_gaps() {
        let body = "<h1 id='one'>One</h1><p>A</p><h1 id='two'>Two</h1>";
        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one", "Text/Body.xhtml#two"]);
        assert!(check(&doc, &data).is_ok());
        data.control.sections[3].range.1 -= 1;
        assert!(check(&doc, &data).is_err());

        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one", "Text/Body.xhtml#two"]);
        data.control.sections[3].range.0 -= 1;
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn d06_segments_must_lie_inside_their_own_section() {
        let body = "<h1 id='one'>One</h1><p>A</p><h1 id='two'>Two</h1>";
        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one", "Text/Body.xhtml#two"]);
        assert!(check(&doc, &data).is_ok());
        let wrong = data.control.sections[3].id.clone();
        data.control.segments[0].section_id = wrong;
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn d08_coincident_destinations_share_one_section_and_keep_every_position() {
        let body = "<p> <span id='outer'><a id='inner'></a>Text</span></p>";
        let targets = ["Text/Body.xhtml#inner", "Text/Body.xhtml#outer"];
        let (doc, data) = valid_targets(body, &targets);
        assert!(check(&doc, &data).is_ok());
        let attached = data
            .control
            .sections
            .iter()
            .find(|section| section.document_id == "d3" && !section.navigation_positions.is_empty())
            .unwrap();
        assert_eq!(attached.navigation_positions, [0, 1]);

        let (doc, mut data) = valid_targets(body, &targets);
        data.control
            .sections
            .iter_mut()
            .find(|section| section.document_id == "d3" && !section.navigation_positions.is_empty())
            .unwrap()
            .navigation_positions
            .clear();
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn d09_prefix_without_navigation_remains_a_section() {
        let body = "<p>Prelude</p><h1 id='one'>One</h1><p>Text</p>";
        let (doc, data) = valid_targets(body, &["Text/Body.xhtml#one"]);
        assert!(check(&doc, &data).is_ok());
        let prefix = data
            .control
            .sections
            .iter()
            .find(|section| section.document_id == "d3" && section.navigation_positions.is_empty())
            .unwrap();
        assert_eq!(prefix.range.0, data.control.documents[2].body.0);
        assert_eq!(data.control.segments[0].section_id, prefix.id);

        let (doc, mut data) = valid_targets(body, &["Text/Body.xhtml#one"]);
        data.control
            .sections
            .iter_mut()
            .find(|section| section.document_id == "d3" && section.navigation_positions.is_empty())
            .unwrap()
            .navigation_positions
            .push(0);
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn d11_segments_cannot_be_reordered_in_the_document() {
        let (doc, mut data) = valid("<p>First.</p><p>Second.</p>");
        assert!(check(&doc, &data).is_ok());
        data.control.segments.swap(0, 1);
        data.source.segments.swap(0, 1);
        assert!(check(&doc, &data).is_err());
    }

    #[test]
    fn m121_a01_linked_note_callout_stays_atomic() {
        let (doc, data) = valid("<p>Word<sup id='n'><a href='#note'>[1]</a></sup>.</p>");
        let sup = doc.ids["n"];
        let index = Index::new(&doc);
        assert!(index.subtree_has_link[sup]);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Word".into()),
                x::Part::Atom("sup1".into()),
                x::Part::Text(".".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_a02_superscript_without_link_stays_inline() {
        let (doc, data) = valid("<p>Word<sup id='n'>2</sup>.</p>");
        let sup = doc.ids["n"];
        let index = Index::new(&doc);
        assert!(!index.subtree_has_link[sup]);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Word".into()),
                x::Part::Open("sup1".into()),
                x::Part::Text("2".into()),
                x::Part::Close("sup1".into()),
                x::Part::Text(".".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_a03_link_deep_in_the_subtree_is_found() {
        let mut content = String::from("<a href='#note'>L</a>");
        for _ in 0..100 {
            content = format!("<span>{content}</span>");
        }
        let (doc, data) = valid(&format!("<p>Before<sup id='n'>{content}</sup>After</p>"));
        let sup = doc.ids["n"];
        let index = Index::new(&doc);
        assert!(index.subtree_has_link[sup]);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Before".into()),
                x::Part::Atom("sup1".into()),
                x::Part::Text("After".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_a04_many_descendants_before_a_link_are_handled() {
        let mut content = String::new();
        for _ in 0..1000 {
            content.push_str("<span>x</span>");
        }
        content.push_str("<a href='#note'>L</a>");
        let (doc, data) = valid(&format!("<p>Before<sup id='n'>{content}</sup>After</p>"));
        let sup = doc.ids["n"];
        let index = Index::new(&doc);
        assert!(index.subtree_has_link[sup]);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Before".into()),
                x::Part::Atom("sup1".into()),
                x::Part::Text("After".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_a05_foreign_link_does_not_change_xhtml_classification() {
        let (doc, data) = valid(
            "<p>Word<sup id='n'><svg xmlns='http://www.w3.org/2000/svg'><a href='#note'>X</a></svg>Tail</sup>.</p>",
        );
        let sup = doc.ids["n"];
        let index = Index::new(&doc);
        assert!(!index.subtree_has_link[sup]);
        assert_eq!(
            data.source.segments[0].parts,
            [
                x::Part::Text("Word".into()),
                x::Part::Open("sup1".into()),
                x::Part::Atom("svg2".into()),
                x::Part::Text("Tail".into()),
                x::Part::Close("sup1".into()),
                x::Part::Text(".".into()),
            ]
        );
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_b01_many_sibling_paragraphs_are_audited() {
        let paragraphs = (0..1000)
            .map(|index| format!("<p>Paragraph {index}.</p>"))
            .collect::<String>();
        let (doc, data) = valid(&format!("<div>{paragraphs}</div>"));
        assert_eq!(data.source.segments.len(), 1000);
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_b02_direct_text_around_nested_blocks_is_audited() {
        let (doc, data) = valid("<div>Before<p>Inside <i>formatted</i>.</p>After</div>");
        assert_eq!(data.source.segments.len(), 3);
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_b03_incomplete_flow_group_is_rejected() {
        let (doc, mut data) = valid("<p><i>One</i>Two</p>");
        let two = doc
            .nodes
            .iter()
            .find(|node| {
                matches!(&node.kind, Kind::Text(chunks) if chunks.iter().any(|chunk| chunk.text == "Two"))
            })
            .unwrap()
            .span;
        data.control.segments[0].range.0 = two.0;
        data.source.segments[0].parts = vec![x::Part::Text("Two".into())];
        let error = check(&doc, &data).unwrap_err();
        assert!(format!("{error}").contains("omits the beginning"));
    }

    #[test]
    fn m121_b04_crossing_flow_groups_is_rejected() {
        let (doc, mut data) = valid("<p>One.</p><p>Two.</p>");
        let end = data.control.segments[1].range.1;
        data.control.segments[0].range.1 = end;
        data.control.segments.remove(1);
        data.source.segments.remove(1);
        data.source.segments[0].parts = vec![x::Part::Text("One.Two.".into())];
        let error = check(&doc, &data).unwrap_err();
        assert!(format!("{error}").contains("crosses flow groups"));
    }

    #[test]
    fn m121_b05_sibling_positions_are_relative_to_each_parent() {
        let (doc, data) = valid(
            "<div><p id='p1'><i id='i'>One</i><b id='b'>Two</b></p><p id='p2'>Three</p></div>",
        );
        let index = Index::new(&doc);
        let p1 = &doc.nodes[doc.ids["p1"]].children;
        let p2 = &doc.nodes[doc.ids["p2"]].children;
        assert_eq!(index.sibling(p1, doc.ids["i"]), Some(0));
        assert_eq!(index.sibling(p1, doc.ids["b"]), Some(1));
        assert_eq!(index.sibling(p2, p2[0]), Some(0));
        assert!(check(&doc, &data).is_ok());
    }

    #[test]
    fn m121_b06_sibling_index_tracks_growing_lists() {
        for count in [250, 1000] {
            let body = (0..count)
                .map(|index| format!("Text {index}.<p>Item {index}.</p>"))
                .collect::<String>();
            let (doc, data) = valid(&format!("<div>{body}</div>"));
            let index = Index::new(&doc);
            assert_eq!(index.sibling_position.len(), doc.nodes.len());
            assert_eq!(data.source.segments.len(), count * 2);
            assert!(check(&doc, &data).is_ok());
        }
    }
}
