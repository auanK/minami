use std::collections::{BTreeMap, BTreeSet};

use crate::{
    MinamiError,
    application::{
        extract,
        validate::{PreparedTranslation, ValidatedTranslation},
    },
    epub::xhtml::{self, ByteSpan, Document, Kind},
    x::{self, Part, Token},
};

pub(crate) struct ReconstructedXhtml {
    pub(crate) document_id: String,
    pub(crate) path: String,
    pub(crate) source: String,
}

pub(crate) fn reconstruct_prepared(
    prepared: &PreparedTranslation,
) -> Result<Vec<ReconstructedXhtml>, MinamiError> {
    reconstruct_documents(prepared.extraction(), prepared.translation())
}

pub(crate) fn reconstruct_documents(
    extraction: &extract::Extraction,
    translation: &ValidatedTranslation,
) -> Result<Vec<ReconstructedXhtml>, MinamiError> {
    let control = &extraction.control;
    if !translation.complete() {
        return Err(invalid(
            "translation responses",
            "reconstruction requires a complete validated translation",
        ));
    }
    if translation.extraction_id() != control.extraction_id {
        return Err(invalid(
            "translation responses",
            "validated translation belongs to a different extraction",
        ));
    }
    if translation.segments().len() != control.segments.len() {
        return Err(invalid(
            "translation coverage",
            "validated translation does not cover the complete extraction",
        ));
    }

    let mut documents = BTreeMap::new();
    for (index, document) in control.documents.iter().enumerate() {
        if documents.insert(document.id.as_str(), index).is_some() {
            return Err(invalid(
                "regenerated extraction",
                "duplicate document identity",
            ));
        }
    }

    let mut ordered = Vec::with_capacity(control.segments.len());
    let mut seen = BTreeSet::new();
    for segment in &control.segments {
        let document_index = *documents.get(segment.document_id.as_str()).ok_or_else(|| {
            invalid(
                "regenerated extraction",
                format!("segment '{}' names an unknown document", segment.id),
            )
        })?;
        if !seen.insert(segment.id.as_str()) {
            return Err(invalid(
                "regenerated extraction",
                "duplicate segment identity",
            ));
        }
        if !translation.segments().contains_key(&segment.id) {
            return Err(invalid(
                "translation coverage",
                "validated segment is missing",
            ));
        }
        ordered.push((document_index, segment));
    }
    ordered.sort_by_key(|(document_index, segment)| {
        (*document_index, segment.range.0, segment.range.1)
    });

    let mut result = Vec::with_capacity(control.documents.len());
    let mut segment_index = 0usize;
    for (document_index, document) in control.documents.iter().enumerate() {
        let start = segment_index;
        while segment_index < ordered.len() && ordered[segment_index].0 == document_index {
            segment_index += 1;
        }
        let segments = ordered[start..segment_index]
            .iter()
            .map(|(_, segment)| *segment)
            .collect::<Vec<_>>();
        let original = extraction.xhtml.get(&document.id).ok_or_else(|| {
            invalid(
                &document.path,
                "original XHTML bytes are absent from the authorized extraction",
            )
        })?;
        let source = reconstruct_document(
            control,
            document,
            original,
            &segments,
            translation.segments(),
        )?;
        result.push(ReconstructedXhtml {
            document_id: document.id.clone(),
            path: document.path.clone(),
            source,
        });
    }
    if segment_index != ordered.len() {
        return Err(invalid(
            "regenerated extraction",
            "segment names an unknown document",
        ));
    }
    Ok(result)
}

fn reconstruct_document(
    control: &x::Control,
    document: &x::Document,
    original: &str,
    segments: &[&x::Segment],
    parts_by_id: &BTreeMap<String, Vec<Part>>,
) -> Result<String, MinamiError> {
    let path = &document.path;
    let body = document.body;
    if body.0 > body.1 || original.get(body.0..body.1).is_none() || body.1 > original.len() {
        return Err(invalid(path, "document body range is invalid"));
    }

    let segment_bytes = segments.iter().try_fold(0usize, |total, segment| {
        let (start, end) = (segment.range.0, segment.range.1);
        total
            .checked_add(
                end.checked_sub(start)
                    .ok_or_else(|| invalid(path, "segment range is inverted"))?,
            )
            .ok_or_else(|| limit(path))
    })?;
    let mut total = original
        .len()
        .checked_sub(segment_bytes)
        .ok_or_else(|| limit(path))?;

    let mut cursor = body.0;
    for segment in segments {
        let (start, end) = (segment.range.0, segment.range.1);
        if segment.document_id != document.id {
            return Err(invalid(
                path,
                format!("segment '{}' belongs to another document", segment.id),
            ));
        }
        if start < cursor || end < start || end > body.1 {
            return Err(invalid(
                path,
                format!("segment '{}' has invalid or overlapping bounds", segment.id),
            ));
        }
        if original.get(start..end).is_none() {
            return Err(invalid(
                path,
                format!("segment '{}' is not on UTF-8 boundaries", segment.id),
            ));
        }
        let replacement = replacement_length(original, segment, &parts_by_id[&segment.id], path)?;
        total = total.checked_add(replacement).ok_or_else(|| limit(path))?;
        if total > xhtml::MAX_BYTES {
            return Err(limit(path));
        }
        cursor = end;
    }

    let mut output = String::new();
    output.try_reserve_exact(total).map_err(|_| limit(path))?;
    append_slice(&mut output, original, ByteSpan(0, body.0), path)?;
    let mut cursor = body.0;
    for segment in segments {
        let (start, end) = (segment.range.0, segment.range.1);
        append_slice(&mut output, original, ByteSpan(cursor, start), path)?;
        let markers = marker_index(segment, path)?;
        for part in &parts_by_id[&segment.id] {
            match part {
                Part::Text(text) => append_escaped(&mut output, text),
                Part::Open(id) | Part::Close(id) | Part::Atom(id) => {
                    let span = token_span(original, segment, &markers, id, part, path)?;
                    append_slice(&mut output, original, span, path)?;
                }
            }
        }
        cursor = end;
    }
    append_slice(&mut output, original, ByteSpan(cursor, body.1), path)?;
    append_slice(
        &mut output,
        original,
        ByteSpan(body.1, original.len()),
        path,
    )?;
    if output.len() != total {
        return Err(invalid(path, "reconstructed XHTML length is inconsistent"));
    }
    verify_document(original, &output, document, control)?;
    Ok(output)
}

fn replacement_length(
    original: &str,
    segment: &x::Segment,
    parts: &[Part],
    path: &str,
) -> Result<usize, MinamiError> {
    let markers = marker_index(segment, path)?;
    let mut total = 0usize;
    for part in parts {
        let length = match part {
            Part::Text(text) => escaped_length(text, path)?,
            Part::Open(id) | Part::Close(id) | Part::Atom(id) => {
                let span = token_span(original, segment, &markers, id, part, path)?;
                span.1 - span.0
            }
        };
        total = total.checked_add(length).ok_or_else(|| limit(path))?;
        if total > xhtml::MAX_BYTES {
            return Err(limit(path));
        }
    }
    Ok(total)
}

fn marker_index<'a>(
    segment: &'a x::Segment,
    path: &str,
) -> Result<BTreeMap<&'a str, &'a Token>, MinamiError> {
    let mut markers = BTreeMap::new();
    for marker in &segment.markers {
        if markers.insert(marker.id.as_str(), &marker.token).is_some() {
            return Err(invalid(
                path,
                format!("segment '{}' has duplicate markers", segment.id),
            ));
        }
    }
    Ok(markers)
}

fn token_span(
    original: &str,
    segment: &x::Segment,
    markers: &BTreeMap<&str, &Token>,
    id: &str,
    part: &Part,
    path: &str,
) -> Result<ByteSpan, MinamiError> {
    let token = markers.get(id).ok_or_else(|| {
        invalid(
            path,
            format!(
                "segment '{}' references unknown marker '{}'",
                segment.id, id
            ),
        )
    })?;
    let span = match (part, *token) {
        (Part::Open(_), Token::Pair { open, .. }) => *open,
        (Part::Close(_), Token::Pair { close, .. }) => *close,
        (Part::Atom(_), Token::Atom { range, .. }) => *range,
        _ => {
            return Err(invalid(
                path,
                format!(
                    "segment '{}' uses marker '{}' with the wrong token kind",
                    segment.id, id
                ),
            ));
        }
    };
    if span.0 > span.1
        || span.0 < segment.range.0
        || span.1 > segment.range.1
        || span.0 == span.1
        || original.get(span.0..span.1).is_none()
    {
        return Err(invalid(
            path,
            format!(
                "segment '{}' has an invalid original span for marker '{}'",
                segment.id, id
            ),
        ));
    }
    Ok(span)
}

fn escaped_length(text: &str, path: &str) -> Result<usize, MinamiError> {
    let mut length = 0usize;
    for value in text.chars() {
        if !xhtml::xml_char(value) {
            return Err(invalid(
                path,
                format!(
                    "translated text contains invalid XML U+{:04X}",
                    value as u32
                ),
            ));
        }
        if xhtml::protected_space(value) {
            return Err(invalid(
                path,
                format!(
                    "translated text contains protected space U+{:04X}",
                    value as u32
                ),
            ));
        }
        let bytes = match value {
            '&' => 5,
            '<' | '>' => 4,
            '\r' => 5,
            _ => value.len_utf8(),
        };
        length = length.checked_add(bytes).ok_or_else(|| limit(path))?;
        if length > xhtml::MAX_BYTES {
            return Err(limit(path));
        }
    }
    Ok(length)
}

fn append_escaped(output: &mut String, text: &str) {
    for value in text.chars() {
        match value {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\r' => output.push_str("&#13;"),
            _ => output.push(value),
        }
    }
}

fn append_slice(
    output: &mut String,
    original: &str,
    span: ByteSpan,
    path: &str,
) -> Result<(), MinamiError> {
    let slice = original
        .get(span.0..span.1)
        .ok_or_else(|| invalid(path, "XHTML byte range is not on UTF-8 boundaries"))?;
    output.push_str(slice);
    Ok(())
}

fn verify_document(
    original: &str,
    reconstructed: &str,
    document: &x::Document,
    control: &x::Control,
) -> Result<(), MinamiError> {
    let original_doc = xhtml::parse(original, &document.path)?;
    let reconstructed_doc = xhtml::parse(reconstructed, &document.path)?;
    if !original_doc.ids.keys().eq(reconstructed_doc.ids.keys()) {
        return Err(invalid(
            &document.path,
            "reconstructed XHTML changed the set of XHTML IDs",
        ));
    }
    if lexical_counts(&original_doc, original)?
        != lexical_counts(&reconstructed_doc, reconstructed)?
    {
        return Err(invalid(
            &document.path,
            "reconstructed XHTML changed protected lexical tokens",
        ));
    }
    if parent_counts(&original_doc, original)? != parent_counts(&reconstructed_doc, reconstructed)?
    {
        return Err(invalid(
            &document.path,
            "reconstructed XHTML changed protected element relationships",
        ));
    }
    for entry in control
        .navigation
        .iter()
        .filter(|entry| entry.path == document.path)
    {
        if let Some(fragment) = entry.fragment.as_deref()
            && !reconstructed_doc.ids.contains_key(fragment)
        {
            return Err(invalid(
                &document.path,
                format!("reconstructed XHTML lost NCX target #{fragment}"),
            ));
        }
    }
    Ok(())
}

fn parent_counts<'a>(
    doc: &Document,
    source: &'a str,
) -> Result<BTreeMap<(Option<&'a str>, &'a str), usize>, MinamiError> {
    let mut counts = BTreeMap::new();
    for node in &doc.nodes {
        let Kind::Element(element) = &node.kind else {
            continue;
        };
        let tag = source
            .get(element.open.0..element.open.1)
            .ok_or_else(|| invalid("XHTML", "invalid lexical token span"))?;
        let parent = node
            .parent
            .and_then(|index| doc.nodes[index].element())
            .map(|element| {
                source
                    .get(element.open.0..element.open.1)
                    .ok_or_else(|| invalid("XHTML", "invalid lexical token span"))
            })
            .transpose()?;
        *counts.entry((parent, tag)).or_insert(0) += 1;
    }
    Ok(counts)
}

fn lexical_counts<'a>(
    doc: &Document,
    source: &'a str,
) -> Result<BTreeMap<&'a str, usize>, MinamiError> {
    let mut counts = BTreeMap::new();
    for node in &doc.nodes {
        match &node.kind {
            Kind::Element(element) => {
                let open = source
                    .get(element.open.0..element.open.1)
                    .ok_or_else(|| invalid("XHTML", "invalid lexical token span"))?;
                *counts.entry(open).or_insert(0) += 1;
                if let Some(close) = element.close {
                    let close = source
                        .get(close.0..close.1)
                        .ok_or_else(|| invalid("XHTML", "invalid lexical token span"))?;
                    *counts.entry(close).or_insert(0) += 1;
                }
            }
            Kind::Comment | Kind::Pi => {
                let token = source
                    .get(node.span.0..node.span.1)
                    .ok_or_else(|| invalid("XHTML", "invalid lexical token span"))?;
                *counts.entry(token).or_insert(0) += 1;
            }
            Kind::Root | Kind::Text(_) => {}
        }
    }
    Ok(counts)
}

fn invalid(path: &str, detail: impl Into<String>) -> MinamiError {
    crate::epub::invalid(path, detail)
}

fn limit(path: &str) -> MinamiError {
    MinamiError::LimitExceeded {
        resource: path.into(),
        limit: "32 MiB reconstructed XHTML document",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        path::{Path, PathBuf},
    };

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        application::{
            support::{Entry, Fixture, OPF, entries, entry, replace},
            validate::{self, ValidationSummary},
        },
        epub::xhtml::{self, Kind},
    };

    const BODY: &str = "OEBPS/Text/Body.xhtml";

    fn wrap(body: &str) -> String {
        format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
    }

    fn ncx(targets: &[&str]) -> String {
        let points = targets
            .iter()
            .enumerate()
            .map(|(index, target)| {
                format!(
                    "<navPoint id='n{index}'><navLabel><text>Section</text></navLabel><content src='{target}'/></navPoint>"
                )
            })
            .collect::<String>();
        format!(
            "<ncx xmlns='http://www.daisy.org/z3986/2005/ncx/' version='2005-1'><navMap>{points}</navMap></ncx>"
        )
    }

    fn book_entries(body: &str) -> Vec<Entry> {
        let mut data = entries();
        replace(&mut data, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(&mut data, "OEBPS/Text/Illustrations.xhtml", wrap(""));
        replace(&mut data, BODY, wrap(body));
        replace(&mut data, "OEBPS/toc.ncx", ncx(&["Text/Body.xhtml"]));
        data
    }

    fn raw_entries(document: &str, targets: &[&str]) -> Vec<Entry> {
        let mut data = entries();
        replace(&mut data, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(&mut data, "OEBPS/Text/Illustrations.xhtml", wrap(""));
        replace(&mut data, BODY, document);
        replace(&mut data, "OEBPS/toc.ncx", ncx(targets));
        data
    }

    struct Case {
        book: Fixture,
        root: tempfile::TempDir,
        work: PathBuf,
        source: Value,
    }

    impl Case {
        fn new(entries: Vec<Entry>) -> Self {
            let book = Fixture::new(entries);
            let root = tempfile::tempdir().unwrap();
            let work = root.path().join("work");
            crate::extract_epub(&book.path, &work).unwrap();
            let source = read_json(&work.join("source.json"));
            Self {
                book,
                root,
                work,
                source,
            }
        }

        fn body(body: &str) -> Self {
            Self::new(book_entries(body))
        }

        fn raw(document: &str, targets: &[&str]) -> Self {
            Self::new(raw_entries(document, targets))
        }

        fn control_path(&self) -> PathBuf {
            self.work.join("control.json")
        }

        fn write(&self, name: &str, value: &Value) -> PathBuf {
            let path = self.root.path().join(name);
            let mut bytes = serde_json::to_vec_pretty(value).unwrap();
            bytes.push(b'\n');
            fs::write(&path, bytes).unwrap();
            path
        }

        fn prepare(&self, response: &Value) -> Result<validate::PreparedTranslation, MinamiError> {
            let path = self.write("response.json", response);
            validate::prepare_reconstruction(&self.book.path, &self.control_path(), &[path])
        }

        fn reconstruct(&self, response: &Value) -> Result<Vec<ReconstructedXhtml>, MinamiError> {
            let prepared = self.prepare(response)?;
            reconstruct_prepared(&prepared)
        }

        fn identity(&self) -> Vec<ReconstructedXhtml> {
            self.reconstruct(&self.source).unwrap()
        }

        fn output(&self, response: &Value) -> String {
            self.reconstruct(response)
                .unwrap()
                .into_iter()
                .find(|document| document.path == BODY)
                .unwrap()
                .source
        }

        fn identity_output(&self) -> String {
            self.output(&self.source)
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn segment_by_text<'a>(case: &'a Case, needle: &str) -> &'a Value {
        case.source["segments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|segment| {
                segment["parts"].as_array().unwrap().iter().any(|part| {
                    part["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(needle))
                })
            })
            .unwrap_or_else(|| panic!("missing source text {needle:?}"))
    }

    fn segment_parts(case: &Case, needle: &str) -> Vec<Value> {
        segment_by_text(case, needle)["parts"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn segment_id(case: &Case, needle: &str) -> String {
        segment_by_text(case, needle)["id"].as_str().unwrap().into()
    }

    fn part_id(parts: &[Value], key: &str, nth: usize) -> String {
        parts
            .iter()
            .filter_map(|part| part[key].as_str())
            .nth(nth)
            .unwrap_or_else(|| panic!("missing {key} {nth}"))
            .into()
    }

    fn with_parts(case: &Case, needle: &str, parts: Vec<Value>) -> Value {
        let id = segment_id(case, needle);
        let mut response = case.source.clone();
        let segment = response["segments"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|segment| segment["id"].as_str() == Some(&id))
            .unwrap();
        segment["parts"] = Value::Array(parts);
        response
    }

    fn decoded_text(source: &str) -> String {
        let doc = xhtml::parse(source, "text.xhtml").unwrap();
        let mut text = String::new();
        for node in &doc.nodes {
            if let Kind::Text(chunks) = &node.kind {
                for chunk in chunks {
                    text.push_str(&chunk.text);
                }
            }
        }
        text
    }

    fn assert_limit(result: Result<Vec<ReconstructedXhtml>, MinamiError>) {
        assert!(matches!(result, Err(MinamiError::LimitExceeded { .. })));
    }

    #[test]
    fn r01_simple_paragraph() {
        let case = Case::body("<p>Hello.</p>");
        let response = with_parts(&case, "Hello.", vec![json!({"text": "Olá."})]);
        assert_eq!(case.output(&response), wrap("<p>Olá.</p>"));
    }

    #[test]
    fn r02_xml_escaping() {
        let case = Case::body("<p>Translate.</p>");
        let response = with_parts(&case, "Translate.", vec![json!({"text": "A < B & C > D"})]);
        let output = case.output(&response);
        assert!(output.contains("<p>A &lt; B &amp; C &gt; D</p>"));
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r03_text_that_looks_like_html_stays_text() {
        let case = Case::body("<p>Translate.</p>");
        let response = with_parts(
            &case,
            "Translate.",
            vec![json!({"text": "Leia <script>alert(1)</script>."})],
        );
        let output = case.output(&response);
        assert!(!output.contains("<script>"));
        assert!(output.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        let doc = xhtml::parse(&output, BODY).unwrap();
        assert!(!doc.nodes.iter().any(|node| {
            node.element()
                .is_some_and(|element| element.name == "script")
        }));
    }

    #[test]
    fn r04_unicode_is_preserved() {
        let case = Case::body("<p>Translate.</p>");
        let response = with_parts(
            &case,
            "Translate.",
            vec![json!({"text": "こんにちは Café 😀"})],
        );
        assert!(case.output(&response).contains("こんにちは Café 😀"));
    }

    #[test]
    fn r05_carriage_return_becomes_a_character_reference() {
        let case = Case::body("<p>Translate.</p>");
        let response = with_parts(&case, "Translate.", vec![json!({"text": "A\rB"})]);
        let output = case.output(&response);
        assert!(output.contains("<p>A&#13;B</p>"));
        assert!(!output.contains('\r'));
    }

    #[test]
    fn r06_translation_may_be_larger_than_the_original() {
        let case = Case::body("<p>Hi.</p>");
        let text = "maior ".repeat(200_000);
        let response = with_parts(&case, "Hi.", vec![json!({"text": text})]);
        let output = case.output(&response);
        assert!(output.len() > 1_000_000);
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r07_output_limit_is_rejected_before_allocation() {
        let case = Case::body("<p>Hi.</p>");
        let response = with_parts(
            &case,
            "Hi.",
            vec![json!({"text": "&".repeat(8 * 1024 * 1024)})],
        );
        assert_limit(case.reconstruct(&response));
    }

    #[test]
    fn r08_simple_inline_tag_uses_original_bytes() {
        let case = Case::body("<p>Hello <i>world</i>.</p>");
        let output = case.identity_output();
        assert!(output.contains("<p>Hello <i>world</i>.</p>"));
    }

    #[test]
    fn r09_attributes_and_quote_style_are_copied() {
        let case = Case::body(
            "<p>Read <a class='note' href='#footnote'>the note</a>.</p><p><span id='footnote'>Note.</span></p>",
        );
        let parts = segment_parts(&case, "the note");
        let open = part_id(&parts, "open", 0);
        let close = part_id(&parts, "close", 0);
        let response = with_parts(
            &case,
            "the note",
            vec![
                json!({"text": "Leia "}),
                json!({"open": open}),
                json!({"text": "a nota"}),
                json!({"close": close}),
                json!({"text": "."}),
            ],
        );
        let output = case.output(&response);
        assert!(output.contains("<a class='note' href='#footnote'>a nota</a>"));
    }

    #[test]
    fn r10_nested_tags_keep_their_parents() {
        let case = Case::body("<p><b>Very <i>small</i></b> room.</p>");
        let parts = segment_parts(&case, "Very");
        let b = part_id(&parts, "open", 0);
        let i = part_id(&parts, "open", 1);
        let response = with_parts(
            &case,
            "Very",
            vec![
                json!({"open": b}),
                json!({"text": "Muito "}),
                json!({"open": i}),
                json!({"text": "pequeno"}),
                json!({"close": i}),
                json!({"close": b}),
                json!({"text": " quarto."}),
            ],
        );
        assert!(
            case.output(&response)
                .contains("<p><b>Muito <i>pequeno</i></b> quarto.</p>")
        );
    }

    #[test]
    fn r11_legitimate_inline_group_move() {
        let case = Case::body("<p>Only <i>then</i> did I understand.</p>");
        let parts = segment_parts(&case, "Only");
        let i = part_id(&parts, "open", 0);
        let response = with_parts(
            &case,
            "Only",
            vec![
                json!({"text": "Eu entendi "}),
                json!({"open": i}),
                json!({"text": "só então"}),
                json!({"close": i}),
                json!({"text": "."}),
            ],
        );
        assert!(
            case.output(&response)
                .contains("<p>Eu entendi <i>só então</i>.</p>")
        );
    }

    #[test]
    fn r12_two_inline_groups_may_swap_when_authorized() {
        let case = Case::body("<p>I <i>said</i> <b>this</b>.</p>");
        let parts = segment_parts(&case, "I ");
        let i = part_id(&parts, "open", 0);
        let b = part_id(&parts, "open", 1);
        let response = with_parts(
            &case,
            "I ",
            vec![
                json!({"text": "Eu "}),
                json!({"open": b}),
                json!({"text": "disse"}),
                json!({"close": b}),
                json!({"text": " "}),
                json!({"open": i}),
                json!({"text": "isto"}),
                json!({"close": i}),
                json!({"text": "."}),
            ],
        );
        let output = case.output(&response);
        assert!(output.contains("<p>Eu <b>disse</b> <i>isto</i>.</p>"));
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r13_br_atom_keeps_its_lexical_form() {
        let case = Case::body("<p>A<br/>B</p>");
        assert!(case.identity_output().contains("<p>A<br/>B</p>"));
    }

    #[test]
    fn r14_image_is_copied_with_attributes() {
        let case = Case::body("<p>A <img src='x.png' alt='X'/> B</p>");
        assert!(
            case.identity_output()
                .contains("<img src='x.png' alt='X'/>")
        );
    }

    #[test]
    fn r15_linked_superscript_is_one_atom() {
        let case = Case::body(
            "<p>See<sup><a href='#note1'>1</a></sup>.</p><p><span id='note1'>Note</span> body.</p>",
        );
        let output = case.identity_output();
        assert!(output.contains("<sup><a href='#note1'>1</a></sup>"));
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r16_svg_subtree_is_copied_byte_for_byte() {
        let case = Case::body(
            "<p>Before <svg xmlns='http://www.w3.org/2000/svg'><path d='M0 0'/></svg> after.</p>",
        );
        let output = case.identity_output();
        assert!(output.contains("<svg xmlns='http://www.w3.org/2000/svg'><path d='M0 0'/></svg>"));
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r17_comment_and_pi_atoms_are_preserved() {
        let case = Case::body("<p>A<!--kept--><?keep value?>B</p>");
        let output = case.identity_output();
        assert!(output.contains("<!--kept-->"));
        assert!(output.contains("<?keep value?>"));
        xhtml::parse(&output, BODY).unwrap();
    }

    fn targeted(body: &str, targets: &[&str]) -> Case {
        let mut data = book_entries(body);
        replace(&mut data, "OEBPS/toc.ncx", ncx(targets));
        Case::new(data)
    }

    #[test]
    fn r18_prefix_and_suffix_bytes_are_unchanged() {
        let before = "<?xml version='1.0'?><html xmlns='http://www.w3.org/1999/xhtml'><head><title>T</title></head><body><p>";
        let after = "</p><!--tail--></body></html>";
        let case = Case::raw(&format!("{before}Hello.{after}"), &["Text/Body.xhtml"]);
        let response = with_parts(&case, "Hello.", vec![json!({"text": "Olá."})]);
        assert_eq!(case.output(&response), format!("{before}Olá.{after}"));
    }

    #[test]
    fn r19_multiple_segments_are_replaced_independently() {
        let case = Case::body("<p>One.</p><p>Two.</p>");
        let mut response = with_parts(&case, "One.", vec![json!({"text": "Um."})]);
        response = {
            let id = segment_id(&case, "Two.");
            for segment in response["segments"].as_array_mut().unwrap() {
                if segment["id"].as_str() == Some(&id) {
                    segment["parts"] = json!([{"text": "Dois."}]);
                }
            }
            response
        };
        let output = case.output(&response);
        assert!(output.contains("<p>Um.</p><p>Dois.</p>"));
    }

    #[test]
    fn r20_larger_first_translation_does_not_shift_the_second() {
        let case = Case::body("<p>Short.</p><p>Second.</p>");
        let mut response = with_parts(
            &case,
            "Short.",
            vec![json!({"text": "Uma tradução consideravelmente maior."})],
        );
        let id = segment_id(&case, "Second.");
        for segment in response["segments"].as_array_mut().unwrap() {
            if segment["id"].as_str() == Some(&id) {
                segment["parts"] = json!([{"text": "Segundo."}]);
            }
        }
        let output = case.output(&response);
        assert!(output.contains("<p>Uma tradução consideravelmente maior.</p><p>Segundo.</p>"));
    }

    #[test]
    fn r21_smaller_first_translation_does_not_shift_the_second() {
        let case = Case::body("<p>A very long original paragraph.</p><p>Second.</p>");
        let mut response = with_parts(&case, "A very long", vec![json!({"text": "A."})]);
        let id = segment_id(&case, "Second.");
        for segment in response["segments"].as_array_mut().unwrap() {
            if segment["id"].as_str() == Some(&id) {
                segment["parts"] = json!([{"text": "B."}]);
            }
        }
        let output = case.output(&response);
        assert!(output.contains("<p>A.</p><p>B.</p>"));
    }

    #[test]
    fn r22_segments_in_shared_sections_are_reconstructed() {
        let case = targeted(
            "<h1 id='one'>One</h1><p>First.</p><h1 id='two'>Two</h1><p>Second.</p>",
            &["Text/Body.xhtml#one", "Text/Body.xhtml#two"],
        );
        let mut response = with_parts(&case, "First.", vec![json!({"text": "Primeiro."})]);
        let id = segment_id(&case, "Second.");
        for segment in response["segments"].as_array_mut().unwrap() {
            if segment["id"].as_str() == Some(&id) {
                segment["parts"] = json!([{"text": "Segundo."}]);
            }
        }
        let output = case.output(&response);
        assert!(output.contains("<p>Primeiro.</p>"));
        assert!(output.contains("<p>Segundo.</p>"));
    }

    #[test]
    fn r23_document_without_segments_is_byte_identical() {
        let case = Case::body("");
        assert_eq!(case.identity_output(), wrap(""));
    }

    #[test]
    fn r24_preserved_regions_remain_identical() {
        let original = "<body>\n<script>var x = 1;</script>\n<style>p { color: red; }</style>\n<!--keep-->\n<p>Hi.</p>\n</body>";
        let document = format!("<html xmlns='http://www.w3.org/1999/xhtml'>{original}</html>");
        let case = Case::raw(&document, &["Text/Body.xhtml"]);
        assert_eq!(case.identity_output(), document);
    }

    #[test]
    fn r25_out_of_document_range_is_rejected_without_panic() {
        let case = Case::body("<p>Hello.</p>");
        let mut prepared = case.prepare(&case.source).unwrap();
        let document_id = prepared
            .extraction()
            .control
            .documents
            .iter()
            .find(|document| document.path == BODY)
            .unwrap()
            .id
            .clone();
        let end = prepared.extraction().xhtml[&document_id].len() + 1;
        prepared.extraction_mut().control.segments[0].range.1 = end;
        assert!(matches!(
            reconstruct_prepared(&prepared),
            Err(MinamiError::InvalidReference { .. })
        ));
    }

    #[test]
    fn r26_overlapping_segments_are_rejected_without_output() {
        let case = Case::body("<p>One.</p><p>Two.</p>");
        let mut prepared = case.prepare(&case.source).unwrap();
        let start = prepared.extraction().control.segments[0].range.0;
        prepared.extraction_mut().control.segments[1].range.0 = start;
        assert!(matches!(
            reconstruct_prepared(&prepared),
            Err(MinamiError::InvalidReference { .. })
        ));
    }

    #[test]
    fn r27_non_utf8_segment_boundary_is_rejected() {
        let case = Case::body("<p>One.</p><p>日本.</p>");
        let mut prepared = case.prepare(&case.source).unwrap();
        prepared.extraction_mut().control.segments[1].range.0 += 1;
        assert!(matches!(
            reconstruct_prepared(&prepared),
            Err(MinamiError::InvalidReference { .. })
        ));
    }

    #[test]
    fn r28_original_entities_are_equivalent_after_reconstruction() {
        let case = Case::body("<p>Caf&#233; &amp; tea.</p>");
        let output = case.identity_output();
        assert_eq!(
            decoded_text(&wrap("<p>Caf&#233; &amp; tea.</p>")),
            decoded_text(&output)
        );
        assert!(output.contains("Café"));
    }

    #[test]
    fn r29_cdata_text_is_preserved_as_character_data() {
        let case = Case::body("<p><![CDATA[A < B]]></p>");
        let output = case.identity_output();
        assert_eq!(
            decoded_text(&wrap("<p><![CDATA[A < B]]></p>")),
            decoded_text(&output)
        );
        xhtml::parse(&output, BODY).unwrap();
    }

    #[test]
    fn r30_original_crlf_outside_segments_is_unchanged() {
        let before = "<html xmlns='http://www.w3.org/1999/xhtml'>\r\n<body><p>";
        let after = "</p>\r\n</body>\r\n</html>";
        let case = Case::raw(&format!("{before}A\r\nB{after}"), &["Text/Body.xhtml"]);
        let response = with_parts(&case, "A", vec![json!({"text": "A\r\nB"})]);
        assert_eq!(case.output(&response), format!("{before}A&#13;\nB{after}"));
    }

    #[test]
    fn r31_protected_spaces_keep_their_original_atoms() {
        let declaration = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">";
        let document = format!("{declaration}{}", wrap("<p>A&nbsp;B&#x2007;C&#x202f;D</p>"));
        let case = Case::raw(&document, &["Text/Body.xhtml"]);
        let output = case.identity_output();
        assert!(output.contains("A&nbsp;B&#x2007;C&#x202f;D"));
        assert!(!output.chars().any(xhtml::protected_space));
    }

    #[test]
    fn r32_bom_is_preserved() {
        let document = format!("\u{feff}{}", wrap("<p>Hi.</p>"));
        let case = Case::raw(&document, &["Text/Body.xhtml"]);
        let output = case.identity_output();
        assert!(output.starts_with('\u{feff}'));
        assert_eq!(output, document);
    }

    #[test]
    fn r33_original_doctype_is_preserved_exactly() {
        let declaration = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">";
        let document = format!("{declaration}{}", wrap("<p>Hi.</p>"));
        let case = Case::raw(&document, &["Text/Body.xhtml"]);
        let output = case.identity_output();
        assert!(output.starts_with(declaration));
        assert_eq!(output.matches("<!DOCTYPE").count(), 1);
    }

    #[test]
    fn r34_no_doctype_is_introduced() {
        assert!(!case_doctype_free_output().contains("<!DOCTYPE"));
    }

    fn case_doctype_free_output() -> String {
        Case::body("<p>Hi.</p>").identity_output()
    }

    #[test]
    fn r35_id_anchor_is_preserved() {
        let case = targeted(
            "<h1 id='chapter'>Chapter</h1><p>Text.</p>",
            &["Text/Body.xhtml#chapter"],
        );
        let output = case.identity_output();
        let doc = xhtml::parse(&output, BODY).unwrap();
        assert!(doc.ids.contains_key("chapter"));
    }

    #[test]
    fn r36_empty_anchor_is_preserved() {
        let case = Case::body("<p>Start <a id='chapter'></a> end.</p>");
        let output = case.identity_output();
        assert!(output.contains("<a id='chapter'></a>"));
    }

    #[test]
    fn r37_multiple_atoms_keep_their_global_order() {
        let case = Case::body("<p>A<br/>B<img src='x.png'/>C<!--k-->D<?pi e?>E</p>");
        let output = case.identity_output();
        let positions = [
            output.find("<br/>").unwrap(),
            output.find("<img src='x.png'/>").unwrap(),
            output.find("<!--k-->").unwrap(),
            output.find("<?pi e?>").unwrap(),
        ];
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn r38_ncx_boundary_target_still_resolves() {
        let case = targeted(
            "<h1 id='one'>Chapter</h1><p>Text.</p>",
            &["Text/Body.xhtml#one"],
        );
        let response = with_parts(&case, "Text.", vec![json!({"text": "Texto."})]);
        let output = case.output(&response);
        assert!(xhtml::parse(&output, BODY).unwrap().ids.contains_key("one"));
        assert!(output.contains("<h1 id='one'>Chapter</h1>"));
    }

    #[test]
    fn r39_multiple_ncx_entries_to_one_document_still_resolve() {
        let case = targeted(
            "<h1 id='one'>One</h1><p>First.</p><h1 id='two'>Two</h1><p>Second.</p>",
            &["Text/Body.xhtml#one", "Text/Body.xhtml#two"],
        );
        let output = case.identity_output();
        let doc = xhtml::parse(&output, BODY).unwrap();
        assert!(doc.ids.contains_key("one"));
        assert!(doc.ids.contains_key("two"));
    }

    #[test]
    fn r40_xhtml_outside_the_spine_is_reconstructed() {
        let mut data = entries();
        replace(&mut data, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(
            &mut data,
            "OEBPS/Text/Illustrations.xhtml",
            wrap("<p>Illustrated.</p>"),
        );
        replace(&mut data, BODY, wrap("<p>Body.</p>"));
        replace(
            &mut data,
            "OEBPS/content.opf",
            OPF.replace(
                "<manifest>",
                "<manifest>\n  <item id=\"extra\" href=\"Text/Extra.xhtml\" media-type=\"application/xhtml+xml\"/>",
            ),
        );
        data.push(entry(
            "OEBPS/Text/Extra.xhtml",
            wrap("<p>Outside spine.</p>"),
        ));
        replace(&mut data, "OEBPS/toc.ncx", ncx(&["Text/Body.xhtml"]));
        let case = Case::new(data);
        let documents = case.identity();
        let extra = documents
            .iter()
            .find(|document| document.path == "OEBPS/Text/Extra.xhtml")
            .unwrap();
        assert!(extra.source.contains("<p>Outside spine.</p>"));
    }

    #[test]
    fn r41_linear_no_documents_are_not_omitted() {
        let mut data = entries();
        replace(&mut data, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(
            &mut data,
            "OEBPS/Text/Illustrations.xhtml",
            wrap("<p>Illustrated.</p>"),
        );
        replace(&mut data, BODY, wrap("<p>Body.</p>"));
        replace(&mut data, "OEBPS/toc.ncx", ncx(&["Text/Body.xhtml"]));
        let case = Case::new(data);
        let documents = case.identity();
        let illustrations = documents
            .iter()
            .find(|document| document.path == "OEBPS/Text/Illustrations.xhtml")
            .unwrap();
        assert!(illustrations.source.contains("<p>Illustrated.</p>"));
    }

    #[test]
    fn r42_allowed_inline_reordering_is_not_rejected() {
        let case = Case::body("<p>I <i>said</i> <b>this</b>.</p>");
        let parts = segment_parts(&case, "I ");
        let i = part_id(&parts, "open", 0);
        let b = part_id(&parts, "open", 1);
        let response = with_parts(
            &case,
            "I ",
            vec![
                json!({"text": "I "}),
                json!({"open": b}),
                json!({"text": "this"}),
                json!({"close": b}),
                json!({"text": " "}),
                json!({"open": i}),
                json!({"text": "said"}),
                json!({"close": i}),
                json!({"text": "."}),
            ],
        );
        let output = case.output(&response);
        assert!(output.contains("<p>I <b>this</b> <i>said</i>.</p>"));
    }

    #[test]
    fn r43_forbidden_anchor_reordering_never_reaches_reconstruction() {
        let case = Case::body("<p><span id='a'>Alpha</span> and <span id='b'>Beta</span>.</p>");
        let parts = segment_parts(&case, "Alpha");
        let a = part_id(&parts, "open", 0);
        let b = part_id(&parts, "open", 1);
        let response = with_parts(
            &case,
            "Alpha",
            vec![
                json!({"open": b}),
                json!({"text": "Beta"}),
                json!({"close": b}),
                json!({"text": " and "}),
                json!({"open": a}),
                json!({"text": "Alpha"}),
                json!({"close": a}),
                json!({"text": "."}),
            ],
        );
        assert!(case.prepare(&response).is_err());
    }

    #[test]
    fn r44_reconstruction_uses_the_validated_owned_parts() {
        let case = Case::body("<p>Hello.</p>");
        let response = with_parts(&case, "Hello.", vec![json!({"text": "Olá."})]);
        let prepared = case.prepare(&response).unwrap();
        assert!(prepared.translation().complete());
        assert_eq!(
            prepared.translation().segments().len(),
            prepared.extraction().control.segments.len()
        );
        let documents = reconstruct_prepared(&prepared).unwrap();
        assert!(
            documents
                .iter()
                .find(|document| document.path == BODY)
                .unwrap()
                .source
                .contains("Olá.")
        );
    }

    #[test]
    fn r45_rejected_translation_cannot_be_reconstructed() {
        let case = Case::body("<p>Hello.</p>");
        let mut response = case.source.clone();
        response["segments"] = json!([]);
        assert!(case.prepare(&response).is_err());
    }

    #[test]
    fn r46_partial_batch_is_never_promoted_to_a_complete_set() {
        let case = Case::body("<p>One.</p><p>Two.</p>");
        let mut partial = case.source.clone();
        partial["segments"].as_array_mut().unwrap().truncate(1);
        let path = case.write("partial.json", &partial);
        let summary = validate::validate_epub(
            &case.book.path,
            &case.control_path(),
            std::slice::from_ref(&path),
            Some(&path),
        )
        .unwrap();
        assert_eq!(
            summary,
            ValidationSummary {
                validated_segments: 1,
                complete: false,
            }
        );
        assert!(
            validate::prepare_reconstruction(&case.book.path, &case.control_path(), &[path])
                .is_err()
        );
    }

    #[test]
    fn r47_changed_response_file_is_not_re_read_after_validation() {
        let case = Case::body("<p>Hello.</p>");
        let validated = with_parts(&case, "Hello.", vec![json!({"text": "Validada."})]);
        let path = case.write("response.json", &validated);
        let prepared = validate::prepare_reconstruction(
            &case.book.path,
            &case.control_path(),
            std::slice::from_ref(&path),
        )
        .unwrap();
        let altered = with_parts(&case, "Hello.", vec![json!({"text": "Alterada."})]);
        let mut bytes = serde_json::to_vec_pretty(&altered).unwrap();
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();
        let documents = reconstruct_prepared(&prepared).unwrap();
        let output = &documents
            .iter()
            .find(|document| document.path == BODY)
            .unwrap()
            .source;
        assert!(output.contains("Validada."));
        assert!(!output.contains("Alterada."));
    }

    #[test]
    fn r48_changed_epub_after_snapshot_is_not_mixed_in() {
        let case = Case::body("<p>Hello.</p>");
        let prepared = case.prepare(&case.source).unwrap();
        let before = reconstruct_prepared(&prepared)
            .unwrap()
            .into_iter()
            .map(|document| (document.path, document.source))
            .collect::<Vec<_>>();
        fs::write(
            &case.book.path,
            crate::application::support::zip_bytes(book_entries("<p>Different.</p>")),
        )
        .unwrap();
        let after = reconstruct_prepared(&prepared)
            .unwrap()
            .into_iter()
            .map(|document| (document.path, document.source))
            .collect::<Vec<_>>();
        assert_eq!(before, after);
        assert!(
            !after
                .iter()
                .any(|(_, source)| source.contains("Different."))
        );
    }

    #[test]
    fn r49_adulterated_control_cannot_build_a_validated_set() {
        let case = Case::body("<p>Hello.</p>");
        let mut control = read_json(&case.control_path());
        control["documents"][0]["path"] = json!("OEBPS/Text/Other.xhtml");
        let mut bytes = serde_json::to_vec_pretty(&control).unwrap();
        bytes.push(b'\n');
        fs::write(case.control_path(), bytes).unwrap();
        assert!(case.prepare(&case.source).is_err());
    }

    #[test]
    fn r50_a_late_failure_yields_no_partial_set() {
        let case = Case::body("<p>Hi.</p>");
        let response = with_parts(
            &case,
            "Hi.",
            vec![json!({"text": "&".repeat(8 * 1024 * 1024)})],
        );
        let prepared = case.prepare(&response).unwrap();
        assert!(reconstruct_prepared(&prepared).is_err());
    }

    #[test]
    fn segment_array_order_does_not_control_byte_order() {
        let case = Case::body("<p>One.</p><p>Two.</p>");
        let mut prepared = case.prepare(&case.source).unwrap();
        prepared.extraction_mut().control.segments.reverse();
        let documents = reconstruct_prepared(&prepared).unwrap();
        let body = documents
            .iter()
            .find(|document| document.path == BODY)
            .unwrap();
        assert!(body.source.contains("<p>One.</p><p>Two.</p>"));
    }

    #[test]
    fn round_trip_identity_preserves_documents_and_lexical_structure() {
        let body = "<p>Hello <i>world</i>.</p><h1 id='chapter'>Chapter</h1><p>A<br/>B <img src='x.png'/> C.</p><p><span id='note'>Note</span> and <b>bold</b>.</p><p>Intro <a id='chapter-anchor'></a> end.</p><p>Read <a href='#note'>the note</a>.</p>";
        let case = targeted(body, &["Text/Body.xhtml#chapter"]);
        let documents = case.identity();
        let expected = [
            ("OEBPS/Text/Cover.xhtml", wrap("")),
            ("OEBPS/Text/Illustrations.xhtml", wrap("")),
            (BODY, wrap(body)),
        ];
        assert_eq!(documents.len(), expected.len());
        for (path, source) in expected {
            let document = documents
                .iter()
                .find(|document| document.path == path)
                .unwrap();
            assert_eq!(document.source, source, "{path}");
            xhtml::parse(&document.source, path).unwrap();
        }
    }

    #[test]
    fn reference_book_identity_round_trip_when_configured() {
        let Ok(epub) = env::var("MINAMI_REFERENCE_EPUB") else {
            return;
        };
        let control = env::var("MINAMI_REFERENCE_CONTROL").unwrap();
        let source = env::var("MINAMI_REFERENCE_SOURCE").unwrap();
        let prepared = validate::prepare_reconstruction(
            Path::new(&epub),
            Path::new(&control),
            &[PathBuf::from(source)],
        )
        .unwrap();
        assert_eq!(
            prepared.translation().segments().len(),
            prepared.extraction().control.segments.len()
        );
        let documents = reconstruct_prepared(&prepared).unwrap();
        assert_eq!(
            documents.len(),
            prepared.extraction().control.documents.len()
        );
        let mut ids = std::collections::BTreeSet::new();
        for document in documents {
            assert!(ids.insert(document.document_id));
            xhtml::parse(&document.source, &document.path).unwrap();
        }
    }
}
