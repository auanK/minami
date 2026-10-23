use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use crate::{
    MinamiError,
    application::{
        audit::{PromotedMarker, SegmentFacts},
        extract,
    },
    epub::{archive::EpubArchive, xhtml},
    x::{self, Part, Token},
};

const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
const MAX_RESPONSE_FILES: usize = 10_000;
const MAX_SEGMENTS: usize = 200_000;
const MAX_PARTS: usize = 1_000_000;
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
// A maximum legal response uses 2,000,000 values in parts, 600,000 in
// segment wrappers, and 4 in the envelope. The 200,000-record control map
// and expected batches stay below that; 3 * MAX_PARTS leaves margin.
const MAX_JSON_VALUES: usize = 3 * MAX_PARTS;
const MAX_JSON_ENTRIES: usize = 3 * MAX_PARTS;
const _: () = assert!(2 * MAX_PARTS + 3 * MAX_SEGMENTS + 4 <= MAX_JSON_VALUES);
const _: () = assert!(2 * MAX_PARTS + 3 * MAX_SEGMENTS + 3 <= MAX_JSON_ENTRIES);
const MAX_PAIR_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationSummary {
    pub validated_segments: usize,
    pub complete: bool,
}

pub(crate) struct ValidatedTranslation {
    extraction_id: String,
    complete: bool,
    segments: BTreeMap<String, Vec<Part>>,
}

impl ValidatedTranslation {
    pub(crate) fn extraction_id(&self) -> &str {
        &self.extraction_id
    }

    pub(crate) fn complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn segments(&self) -> &BTreeMap<String, Vec<Part>> {
        &self.segments
    }
}

pub(crate) struct PreparedTranslation {
    extraction: extract::Extraction,
    translation: ValidatedTranslation,
    archive: EpubArchive,
}

impl PreparedTranslation {
    pub(crate) fn extraction(&self) -> &extract::Extraction {
        &self.extraction
    }

    pub(crate) fn translation(&self) -> &ValidatedTranslation {
        &self.translation
    }

    pub(crate) fn into_archive(self) -> EpubArchive {
        self.archive
    }

    #[cfg(test)]
    pub(crate) fn extraction_mut(&mut self) -> &mut extract::Extraction {
        &mut self.extraction
    }
}

pub fn validate_epub(
    epub: &Path,
    control: &Path,
    translations: &[PathBuf],
    expected_batch: Option<&Path>,
) -> Result<ValidationSummary, MinamiError> {
    let prepared = validate_epub_internal(epub, control, translations, expected_batch)?;
    Ok(ValidationSummary {
        validated_segments: prepared.translation.segments.len(),
        complete: prepared.translation.complete,
    })
}

pub(crate) fn prepare_reconstruction(
    epub: &Path,
    control: &Path,
    translations: &[PathBuf],
) -> Result<PreparedTranslation, MinamiError> {
    let prepared = validate_epub_internal(epub, control, translations, None)?;
    if !prepared.translation.complete {
        return Err(invalid(
            "translation responses",
            "reconstruction requires a complete validated translation",
        ));
    }
    Ok(prepared)
}

fn validate_epub_internal(
    epub: &Path,
    control: &Path,
    translations: &[PathBuf],
    expected_batch: Option<&Path>,
) -> Result<PreparedTranslation, MinamiError> {
    if expected_batch.is_some() && translations.len() != 1 {
        return Err(invalid(
            "expected batch",
            "batch validation requires exactly one --translation file",
        ));
    }
    if translations.is_empty() {
        return Err(invalid(
            "translation responses",
            "at least one --translation file is required",
        ));
    }
    if translations.len() > MAX_RESPONSE_FILES {
        return Err(MinamiError::LimitExceeded {
            resource: "translation responses".into(),
            limit: "10000 response files",
        });
    }

    let control_resource = control.display().to_string();
    let supplied = read_json(control, MAX_JSON_BYTES, "64 MiB control file")?;
    let (data, archive) = extract::extract_with_archive(epub)?;
    let regenerated_bytes = extract::encode(&data.control)?;
    let regenerated = parse_json(&regenerated_bytes, "regenerated control")?;
    if supplied != regenerated {
        return Err(invalid(
            &control_resource,
            "control.json does not match the extraction regenerated from the selected EPUB",
        ));
    }

    let source_by_id: BTreeMap<_, _> = data
        .source
        .segments
        .iter()
        .map(|segment| (segment.id.as_str(), segment))
        .collect();
    let expectations = expectations(&data)?;

    let expected_ids = match expected_batch {
        Some(path) => expected_batch_ids(path, &data, &source_by_id)?,
        None => data
            .control
            .segments
            .iter()
            .map(|segment| segment.id.clone())
            .collect(),
    };
    let expected_set: BTreeSet<&str> = expected_ids.iter().map(String::as_str).collect();

    let mut totals = Totals::default();
    let mut total_bytes = 0usize;
    let mut parts_by_id = BTreeMap::new();
    let mut origin_by_id = BTreeMap::new();
    for path in translations {
        let resource = path.display().to_string();
        let bytes = read_limited(path, MAX_JSON_BYTES, "64 MiB response file")?;
        total_bytes =
            total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| MinamiError::LimitExceeded {
                    resource: "translation responses".into(),
                    limit: "256 MiB total response bytes",
                })?;
        if total_bytes > MAX_TOTAL_RESPONSE_BYTES {
            return Err(MinamiError::LimitExceeded {
                resource: "translation responses".into(),
                limit: "256 MiB total response bytes",
            });
        }
        let json = parse_json(&bytes, &resource)?;
        let envelope = parse_envelope(&json, &resource, &mut totals)?;
        if envelope.format_version != x::VERSION {
            return Err(invalid(
                &resource,
                format!("format_version must be '{}'", x::VERSION),
            ));
        }
        if envelope.extraction_id != data.control.extraction_id {
            return Err(invalid(
                &resource,
                "extraction_id does not match the regenerated extraction",
            ));
        }
        for segment in envelope.segments {
            if !expected_set.contains(segment.id.as_str()) {
                return Err(invalid(
                    &resource,
                    format!("unknown segment ID '{}'", safe_id(&segment.id)),
                ));
            }
            if let Some(previous) = origin_by_id.insert(segment.id.clone(), resource.clone()) {
                let context = data
                    .control
                    .segments
                    .iter()
                    .find(|expected| expected.id == segment.id)
                    .map(|expected| {
                        segment_context(
                            expected,
                            document_path(&data.control, &expected.document_id),
                        )
                    })
                    .unwrap_or_else(|| format!("segment '{}'", safe_id(&segment.id)));
                return Err(invalid(
                    &resource,
                    format!("{context} appears in both '{previous}' and '{resource}'"),
                ));
            }
            parts_by_id.insert(segment.id, segment.parts);
        }
    }

    let missing: Vec<_> = expected_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !parts_by_id.contains_key(*id))
        .collect();
    if !missing.is_empty() {
        let shown = missing
            .iter()
            .take(5)
            .map(|id| {
                data.control
                    .segments
                    .iter()
                    .find(|segment| segment.id == *id)
                    .map(|segment| {
                        segment_context(segment, document_path(&data.control, &segment.document_id))
                    })
                    .unwrap_or_else(|| format!("segment '{}'", safe_id(id)))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let more = missing.len().saturating_sub(5);
        let suffix = if more == 0 {
            String::new()
        } else {
            format!("; and {more} more")
        };
        return Err(invalid(
            "translation coverage",
            format!(
                "translation is missing {} segment(s): {shown}{suffix}",
                missing.len()
            ),
        ));
    }

    for id in &expected_ids {
        let expected = expectations
            .get(id.as_str())
            .ok_or_else(|| invalid("regenerated extraction", "segment expectation is missing"))?;
        let target = parts_by_id
            .get(id)
            .ok_or_else(|| invalid("translation coverage", "validated segment is missing"))?;
        let resource = origin_by_id
            .get(id)
            .ok_or_else(|| invalid("translation coverage", "segment origin is missing"))?;
        validate_segment(expected, target, resource)?;
    }

    let translation = ValidatedTranslation {
        extraction_id: data.control.extraction_id.clone(),
        complete: expected_batch.is_none(),
        segments: parts_by_id,
    };
    Ok(PreparedTranslation {
        extraction: data,
        translation,
        archive,
    })
}

struct ExpectedSegment<'a> {
    control: &'a x::Segment,
    source: &'a x::SourceSegment,
    facts: &'a SegmentFacts,
    document_path: &'a str,
}

fn expectations(
    data: &extract::Extraction,
) -> Result<BTreeMap<&str, ExpectedSegment<'_>>, MinamiError> {
    if data.control.segments.len() != data.source.segments.len() {
        return Err(invalid(
            "regenerated extraction",
            "control and source segment counts differ",
        ));
    }
    let mut result = BTreeMap::new();
    for (control, source) in data.control.segments.iter().zip(&data.source.segments) {
        if control.id != source.id || result.contains_key(control.id.as_str()) {
            return Err(invalid(
                "regenerated extraction",
                "segment identity is inconsistent",
            ));
        }
        let facts = data.facts.segments.get(&control.id).ok_or_else(|| {
            invalid(
                "regenerated extraction",
                "segment lexical facts are missing",
            )
        })?;
        let document_path = data
            .control
            .documents
            .iter()
            .find(|document| document.id == control.document_id)
            .map(|document| document.path.as_str())
            .ok_or_else(|| invalid("regenerated extraction", "segment document is missing"))?;
        let context = segment_context(control, document_path);
        for (part_index, part) in source.parts.iter().enumerate() {
            if let Part::Text(text) = part {
                check_text(text, "regenerated extraction", &context, part_index)?;
            }
        }
        result.insert(
            control.id.as_str(),
            ExpectedSegment {
                control,
                source,
                facts,
                document_path,
            },
        );
    }
    Ok(result)
}

fn expected_batch_ids(
    path: &Path,
    data: &extract::Extraction,
    source_by_id: &BTreeMap<&str, &x::SourceSegment>,
) -> Result<Vec<String>, MinamiError> {
    let resource = path.display().to_string();
    let bytes = read_limited(path, MAX_JSON_BYTES, "64 MiB batch file")?;
    let json = parse_json(&bytes, &resource)?;
    let mut totals = Totals::default();
    let envelope = parse_envelope(&json, &resource, &mut totals)?;
    if envelope.format_version != x::VERSION {
        return Err(invalid(
            &resource,
            format!("format_version must be '{}'", x::VERSION),
        ));
    }
    if envelope.extraction_id != data.control.extraction_id {
        return Err(invalid(
            &resource,
            "extraction_id does not match the regenerated extraction",
        ));
    }
    let mut ids = Vec::with_capacity(envelope.segments.len());
    let mut seen = BTreeSet::new();
    for segment in envelope.segments {
        if !seen.insert(segment.id.clone()) {
            return Err(invalid(
                &resource,
                format!("expected batch repeats segment '{}'", safe_id(&segment.id)),
            ));
        }
        let source = source_by_id.get(segment.id.as_str()).ok_or_else(|| {
            invalid(
                &resource,
                format!(
                    "expected batch contains unknown segment '{}'",
                    safe_id(&segment.id)
                ),
            )
        })?;
        if segment.parts != source.parts {
            return Err(invalid(
                &resource,
                format!(
                    "expected batch segment '{}' differs from the regenerated extraction",
                    safe_id(&segment.id)
                ),
            ));
        }
        ids.push(segment.id);
    }
    Ok(ids)
}

struct Frame<'a> {
    id: &'a str,
    has_text: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EventKind {
    Open,
    Close,
    Atom,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Event {
    kind: EventKind,
    id: String,
}

fn event(part: &Part) -> Option<Event> {
    match part {
        Part::Open(id) => Some(Event {
            kind: EventKind::Open,
            id: id.clone(),
        }),
        Part::Close(id) => Some(Event {
            kind: EventKind::Close,
            id: id.clone(),
        }),
        Part::Atom(id) => Some(Event {
            kind: EventKind::Atom,
            id: id.clone(),
        }),
        Part::Text(_) => None,
    }
}

fn validate_segment(
    expected: &ExpectedSegment<'_>,
    target: &[Part],
    resource: &str,
) -> Result<(), MinamiError> {
    let context = segment_context(expected.control, expected.document_path);
    let mut markers = BTreeMap::new();
    for marker in &expected.control.markers {
        if markers.insert(marker.id.as_str(), &marker.token).is_some() {
            return Err(invalid(
                resource,
                format!("{context}: duplicate marker in regenerated control"),
            ));
        }
    }

    let mut opened = BTreeSet::new();
    let mut closed = BTreeSet::new();
    let mut atoms = BTreeSet::new();
    let mut target_atoms = Vec::new();
    let mut target_events = Vec::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut segment_has_text = false;
    let mut target_linguistic_before = Vec::new();
    let mut linguistic_seen = false;

    for (part_index, part) in target.iter().enumerate() {
        match part {
            Part::Text(text) => {
                check_text(text, resource, &context, part_index)?;
                if text.chars().any(|value| !xhtml::xml_space(value)) {
                    segment_has_text = true;
                    linguistic_seen = true;
                    if let Some(frame) = stack.last_mut() {
                        frame.has_text = true;
                    }
                }
            }
            Part::Open(id) => {
                target_linguistic_before.push(linguistic_seen);
                let token = markers.get(id.as_str()).ok_or_else(|| {
                    invalid(
                        resource,
                        format!(
                            "{context}: unknown open marker '{}' at part {part_index}",
                            safe_id(id)
                        ),
                    )
                })?;
                let Token::Pair { parent, .. } = token else {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' is not a paired marker at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                };
                if !opened.insert(id.as_str()) {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' opens more than once at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                if stack.len() >= MAX_PAIR_DEPTH {
                    return Err(MinamiError::LimitExceeded {
                        resource: resource.into(),
                        limit: "64 paired marker nesting levels",
                    });
                }
                let actual_parent = stack.last().map(|frame| frame.id);
                if actual_parent != parent.as_deref() {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' has the wrong parent at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                stack.push(Frame {
                    id,
                    has_text: false,
                });
                target_events.push(Event {
                    kind: EventKind::Open,
                    id: id.clone(),
                });
            }
            Part::Close(id) => {
                target_linguistic_before.push(linguistic_seen);
                let token = markers.get(id.as_str()).ok_or_else(|| {
                    invalid(
                        resource,
                        format!(
                            "{context}: unknown close marker '{}' at part {part_index}",
                            safe_id(id)
                        ),
                    )
                })?;
                if !matches!(token, Token::Pair { .. }) {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' is not a paired marker at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                if !closed.insert(id.as_str()) {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' closes more than once at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                let Some(frame) = stack.pop() else {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' closes without an open marker at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                };
                if frame.id != id {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' closes while '{}' is open at part {part_index}",
                            safe_id(id),
                            safe_id(frame.id)
                        ),
                    ));
                }
                if !frame.has_text {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: paired marker '{}' has no non-whitespace text",
                            safe_id(id)
                        ),
                    ));
                }
                if let Some(parent) = stack.last_mut() {
                    parent.has_text |= frame.has_text;
                }
                target_events.push(Event {
                    kind: EventKind::Close,
                    id: id.clone(),
                });
            }
            Part::Atom(id) => {
                target_linguistic_before.push(linguistic_seen);
                let token = markers.get(id.as_str()).ok_or_else(|| {
                    invalid(
                        resource,
                        format!(
                            "{context}: unknown atom marker '{}' at part {part_index}",
                            safe_id(id)
                        ),
                    )
                })?;
                let Token::Atom { parent, .. } = token else {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: marker '{}' is not an atom at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                };
                if !atoms.insert(id.as_str()) {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: atom marker '{}' occurs more than once at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                let actual_parent = stack.last().map(|frame| frame.id);
                if actual_parent != parent.as_deref() {
                    return Err(invalid(
                        resource,
                        format!(
                            "{context}: atom marker '{}' has the wrong parent at part {part_index}",
                            safe_id(id)
                        ),
                    ));
                }
                target_atoms.push(id.as_str());
                target_events.push(Event {
                    kind: EventKind::Atom,
                    id: id.clone(),
                });
            }
        }
    }

    if let Some(frame) = stack.last() {
        return Err(invalid(
            resource,
            format!("{context}: marker '{}' is not closed", safe_id(frame.id)),
        ));
    }
    for marker in &expected.control.markers {
        let present = match &marker.token {
            Token::Pair { .. } => {
                opened.contains(marker.id.as_str()) && closed.contains(marker.id.as_str())
            }
            Token::Atom { .. } => atoms.contains(marker.id.as_str()),
        };
        if !present {
            return Err(invalid(
                resource,
                format!(
                    "{context}: required marker '{}' is missing",
                    safe_id(&marker.id)
                ),
            ));
        }
    }
    if !segment_has_text {
        return Err(invalid(
            resource,
            format!("{context}: target segment has no non-whitespace text"),
        ));
    }

    let source_atoms: Vec<_> = expected
        .source
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Atom(id) => Some(id.as_str()),
            Part::Text(_) | Part::Open(_) | Part::Close(_) => None,
        })
        .collect();
    if target_atoms != source_atoms {
        return Err(invalid(
            resource,
            format!("{context}: global atom order differs from the original"),
        ));
    }

    let source_events: Vec<_> = expected.source.parts.iter().filter_map(event).collect();
    if source_events.len() != target_events.len() {
        return Err(invalid(
            resource,
            format!("{context}: marker event count differs from the original"),
        ));
    }
    check_anchors(
        &source_events,
        &target_events,
        expected.facts,
        &markers,
        resource,
        &context,
    )?;
    check_ncx(
        &target_events,
        &target_linguistic_before,
        &expected.facts.promoted,
        &markers,
        resource,
        &context,
    )?;
    Ok(())
}

fn check_anchors(
    source: &[Event],
    target: &[Event],
    facts: &SegmentFacts,
    markers: &BTreeMap<&str, &Token>,
    resource: &str,
    context: &str,
) -> Result<(), MinamiError> {
    if facts.anchors.is_empty() {
        return Ok(());
    }
    let source_positions: BTreeMap<_, _> = source
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, event)| (event, index))
        .collect();
    let target_positions: BTreeMap<_, _> = target
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, event)| (event, index))
        .collect();
    let mut cuts = BTreeSet::new();
    for id in &facts.anchors {
        let token = markers.get(id.as_str()).ok_or_else(|| {
            invalid(
                resource,
                format!("{context}: anchor marker '{}' is missing", safe_id(id)),
            )
        })?;
        let kinds: &[EventKind] = match token {
            Token::Pair { .. } => &[EventKind::Open, EventKind::Close],
            Token::Atom { .. } => &[EventKind::Atom],
        };
        for &kind in kinds {
            let event = Event {
                kind,
                id: id.clone(),
            };
            let source_index = *source_positions.get(&event).ok_or_else(|| {
                invalid(
                    resource,
                    format!(
                        "{context}: anchor event for '{}' is missing from the source",
                        safe_id(id)
                    ),
                )
            })?;
            let target_index = *target_positions.get(&event).ok_or_else(|| {
                invalid(
                    resource,
                    format!(
                        "{context}: anchor event for '{}' is missing from the response",
                        safe_id(id)
                    ),
                )
            })?;
            if source_index != target_index {
                return Err(invalid(
                    resource,
                    format!(
                        "{context}: XHTML ID anchor '{}' was moved across another event",
                        safe_id(id)
                    ),
                ));
            }
            cuts.insert(source_index);
        }
    }
    let mut difference = BTreeMap::<Event, isize>::new();
    let mut nonzero = 0usize;
    for index in 0..=source.len() {
        if cuts.contains(&index) && nonzero != 0 {
            return Err(invalid(
                resource,
                format!("{context}: XHTML ID anchor placement differs from the original"),
            ));
        }
        if index == source.len() {
            break;
        }
        adjust(&mut difference, &mut nonzero, source[index].clone(), 1);
        adjust(&mut difference, &mut nonzero, target[index].clone(), -1);
    }
    Ok(())
}

fn adjust(
    difference: &mut BTreeMap<Event, isize>,
    nonzero: &mut usize,
    event: Event,
    delta: isize,
) {
    let value = difference.entry(event).or_insert(0);
    let before = *value;
    *value += delta;
    if before == 0 && *value != 0 {
        *nonzero += 1;
    } else if before != 0 && *value == 0 {
        *nonzero -= 1;
    }
}

fn check_ncx(
    target: &[Event],
    linguistic_before: &[bool],
    promoted: &[PromotedMarker],
    markers: &BTreeMap<&str, &Token>,
    resource: &str,
    context: &str,
) -> Result<(), MinamiError> {
    let positions: BTreeMap<_, _> = target
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, event)| (event, index))
        .collect();
    for boundary in promoted {
        let token = markers.get(boundary.id.as_str()).ok_or_else(|| {
            invalid(
                resource,
                format!(
                    "{context}: NCX boundary marker '{}' is missing",
                    safe_id(&boundary.id)
                ),
            )
        })?;
        let event = match token {
            Token::Pair { .. } => Event {
                kind: EventKind::Open,
                id: boundary.id.clone(),
            },
            Token::Atom { .. } => Event {
                kind: EventKind::Atom,
                id: boundary.id.clone(),
            },
        };
        let index = positions.get(&event).copied().ok_or_else(|| {
            invalid(
                resource,
                format!(
                    "{context}: NCX boundary marker '{}' is missing from the response",
                    safe_id(&boundary.id)
                ),
            )
        })?;
        if index != boundary.ancestors.len() {
            return Err(invalid(
                resource,
                format!(
                    "{context}: NCX boundary marker '{}' has a non-ancestor event before it",
                    safe_id(&boundary.id)
                ),
            ));
        }
        if linguistic_before.get(index).copied().unwrap_or(true) {
            return Err(invalid(
                resource,
                format!(
                    "{context}: NCX boundary marker '{}' follows linguistic text",
                    safe_id(&boundary.id)
                ),
            ));
        }
    }
    Ok(())
}

fn check_text(
    text: &str,
    resource: &str,
    context: &str,
    part_index: usize,
) -> Result<(), MinamiError> {
    for value in text.chars() {
        if !xhtml::xml_char(value) {
            return Err(invalid(
                resource,
                format!(
                    "{context}: invalid XML character U+{:04X} in text part {part_index}",
                    value as u32
                ),
            ));
        }
        if xhtml::protected_space(value) {
            return Err(invalid(
                resource,
                format!(
                    "{context}: protected space U+{:04X} is forbidden in text part {part_index}",
                    value as u32
                ),
            ));
        }
    }
    Ok(())
}

fn segment_context(segment: &x::Segment, document_path: &str) -> String {
    format!(
        "segment '{}' (document '{}', path '{}', section '{}')",
        segment.id, segment.document_id, document_path, segment.section_id
    )
}

fn document_path<'a>(control: &'a x::Control, document_id: &str) -> &'a str {
    control
        .documents
        .iter()
        .find(|document| document.id == document_id)
        .map_or("<unknown-document>", |document| document.path.as_str())
}

fn safe_id(id: &str) -> &str {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        id
    } else {
        "<invalid-id>"
    }
}

#[derive(Default)]
struct Totals {
    segments: usize,
    parts: usize,
    text_bytes: usize,
}

impl Totals {
    fn add_segment(&mut self, resource: &str) -> Result<(), MinamiError> {
        self.segments = self
            .segments
            .checked_add(1)
            .ok_or_else(|| MinamiError::LimitExceeded {
                resource: resource.into(),
                limit: "200000 response segments",
            })?;
        if self.segments > MAX_SEGMENTS {
            return Err(MinamiError::LimitExceeded {
                resource: resource.into(),
                limit: "200000 response segments",
            });
        }
        Ok(())
    }

    fn add_part(&mut self, resource: &str) -> Result<(), MinamiError> {
        self.parts = self
            .parts
            .checked_add(1)
            .ok_or_else(|| MinamiError::LimitExceeded {
                resource: resource.into(),
                limit: "1000000 response parts",
            })?;
        if self.parts > MAX_PARTS {
            return Err(MinamiError::LimitExceeded {
                resource: resource.into(),
                limit: "1000000 response parts",
            });
        }
        Ok(())
    }

    fn add_text(&mut self, bytes: usize, resource: &str) -> Result<(), MinamiError> {
        self.text_bytes =
            self.text_bytes
                .checked_add(bytes)
                .ok_or_else(|| MinamiError::LimitExceeded {
                    resource: resource.into(),
                    limit: "64 MiB decoded response text",
                })?;
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err(MinamiError::LimitExceeded {
                resource: resource.into(),
                limit: "64 MiB decoded response text",
            });
        }
        Ok(())
    }
}

struct ParsedEnvelope {
    format_version: String,
    extraction_id: String,
    segments: Vec<x::SourceSegment>,
}

fn parse_envelope(
    value: &Json,
    resource: &str,
    totals: &mut Totals,
) -> Result<ParsedEnvelope, MinamiError> {
    let envelope = object(value, resource, "X envelope")?;
    exact_fields(
        envelope,
        &["format_version", "extraction_id", "segments"],
        resource,
        "X envelope",
    )?;
    let format_version = string_field(envelope, "format_version", resource)?.to_owned();
    let extraction_id = string_field(envelope, "extraction_id", resource)?.to_owned();
    let segments = array_field(envelope, "segments", resource)?;

    let mut parsed_segments = Vec::with_capacity(segments.len());
    let mut seen = BTreeSet::new();
    for segment in segments {
        let fields = object(segment, resource, "segment")?;
        exact_fields(fields, &["id", "parts"], resource, "segment")?;
        let id = string_field(fields, "id", resource)?.to_owned();
        if !seen.insert(id.clone()) {
            return Err(invalid(
                resource,
                format!("duplicate segment ID '{}'", safe_id(&id)),
            ));
        }
        let parts = array_field(fields, "parts", resource)?;
        let mut parsed_parts = Vec::with_capacity(parts.len());
        for part in parts {
            totals.add_part(resource)?;
            let fields = object(part, resource, "part")?;
            if fields.len() != 1 {
                return Err(invalid(resource, "each part must have exactly one field"));
            }
            let (name, value) = fields
                .iter()
                .next()
                .ok_or_else(|| invalid(resource, "each part must have exactly one field"))?;
            let Json::String(text) = value else {
                return Err(invalid(
                    resource,
                    format!("part field '{name}' must contain a string"),
                ));
            };
            let part = match name.as_str() {
                "text" => {
                    totals.add_text(text.len(), resource)?;
                    Part::Text(text.clone())
                }
                "open" => Part::Open(text.clone()),
                "close" => Part::Close(text.clone()),
                "atom" => Part::Atom(text.clone()),
                _ => return Err(invalid(resource, "unknown part field")),
            };
            parsed_parts.push(part);
        }
        totals.add_segment(resource)?;
        parsed_segments.push(x::SourceSegment {
            id,
            parts: parsed_parts,
        });
    }
    Ok(ParsedEnvelope {
        format_version,
        extraction_id,
        segments: parsed_segments,
    })
}

fn object<'a>(
    value: &'a Json,
    resource: &str,
    what: &str,
) -> Result<&'a BTreeMap<String, Json>, MinamiError> {
    let Json::Object(object) = value else {
        return Err(invalid(resource, format!("{what} must be an object")));
    };
    Ok(object)
}

fn exact_fields(
    object: &BTreeMap<String, Json>,
    fields: &[&str],
    resource: &str,
    what: &str,
) -> Result<(), MinamiError> {
    if object.len() != fields.len() {
        let unknown = object.keys().any(|key| !fields.contains(&key.as_str()));
        let detail = if unknown {
            format!("{what} has an unknown field")
        } else {
            format!("{what} is missing a required field")
        };
        return Err(invalid(resource, detail));
    }
    if let Some(missing) = fields.iter().find(|field| !object.contains_key(**field)) {
        return Err(invalid(
            resource,
            format!("{what} is missing required field '{missing}'"),
        ));
    }
    Ok(())
}

fn string_field<'a>(
    object: &'a BTreeMap<String, Json>,
    name: &str,
    resource: &str,
) -> Result<&'a str, MinamiError> {
    match object.get(name) {
        Some(Json::String(value)) => Ok(value),
        Some(_) => Err(invalid(
            resource,
            format!("field '{name}' must be a string"),
        )),
        None => Err(invalid(
            resource,
            format!("missing required field '{name}'"),
        )),
    }
}

fn array_field<'a>(
    object: &'a BTreeMap<String, Json>,
    name: &str,
    resource: &str,
) -> Result<&'a [Json], MinamiError> {
    match object.get(name) {
        Some(Json::Array(value)) => Ok(value),
        Some(_) => Err(invalid(
            resource,
            format!("field '{name}' must be an array"),
        )),
        None => Err(invalid(
            resource,
            format!("missing required field '{name}'"),
        )),
    }
}

fn read_json(path: &Path, limit: usize, label: &'static str) -> Result<Json, MinamiError> {
    let resource = path.display().to_string();
    let bytes = read_limited(path, limit, label)?;
    parse_json(&bytes, &resource)
}

fn read_limited(path: &Path, limit: usize, label: &'static str) -> Result<Vec<u8>, MinamiError> {
    let resource = path.display().to_string();
    let file =
        File::open(path).map_err(|error| io_error("opening JSON input", &resource, error))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("reading JSON input", &resource, error))?;
    if bytes.len() > limit {
        return Err(MinamiError::LimitExceeded {
            resource,
            limit: label,
        });
    }
    Ok(bytes)
}

fn io_error(operation: &'static str, resource: &str, source: io::Error) -> MinamiError {
    MinamiError::Io {
        operation,
        resource: resource.into(),
        source,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

#[derive(Clone, Copy)]
struct ParseBudget {
    remaining_values: usize,
    remaining_entries: usize,
}

impl ParseBudget {
    const NORMAL: Self = Self::new(MAX_JSON_VALUES, MAX_JSON_ENTRIES);

    const fn new(remaining_values: usize, remaining_entries: usize) -> Self {
        Self {
            remaining_values,
            remaining_entries,
        }
    }

    fn take_value(&mut self) -> bool {
        let Some(remaining) = self.remaining_values.checked_sub(1) else {
            return false;
        };
        self.remaining_values = remaining;
        true
    }

    fn take_entry(&mut self) -> bool {
        let Some(remaining) = self.remaining_entries.checked_sub(1) else {
            return false;
        };
        self.remaining_entries = remaining;
        true
    }
}

fn parse_json(bytes: &[u8], resource: &str) -> Result<Json, MinamiError> {
    parse_json_with_budget(bytes, resource, ParseBudget::NORMAL)
}

fn parse_json_with_budget(
    bytes: &[u8],
    resource: &str,
    budget: ParseBudget,
) -> Result<Json, MinamiError> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(invalid(resource, "UTF-8 BOM is not allowed"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid(resource, "input is not valid UTF-8"))?;
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        offset: 0,
        resource,
        budget,
    };
    let value = parser.value(0)?;
    parser.skip_space();
    if parser.offset != text.len() {
        return Err(invalid(
            resource,
            format!("trailing data at byte {}", parser.offset),
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    offset: usize,
    resource: &'a str,
    budget: ParseBudget,
}

impl Parser<'_> {
    fn error(&self, detail: &'static str) -> MinamiError {
        invalid(
            self.resource,
            format!("invalid JSON at byte {}: {detail}", self.offset),
        )
    }

    fn budget_limit(&self, limit: &'static str) -> MinamiError {
        MinamiError::LimitExceeded {
            resource: self.resource.into(),
            limit,
        }
    }

    fn depth_limit(&self) -> MinamiError {
        self.budget_limit("64 JSON nesting levels")
    }

    fn value_budget(&mut self) -> Result<(), MinamiError> {
        if self.budget.take_value() {
            Ok(())
        } else {
            Err(self.budget_limit("JSON value budget"))
        }
    }

    fn entry_budget(&mut self) -> Result<(), MinamiError> {
        if self.budget.take_entry() {
            Ok(())
        } else {
            Err(self.budget_limit("JSON entry budget"))
        }
    }

    fn skip_space(&mut self) {
        while matches!(
            self.bytes.get(self.offset),
            Some(b' ' | b'\t' | b'\n' | b'\r')
        ) {
            self.offset += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, MinamiError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.depth_limit());
        }
        self.value_budget()?;
        self.skip_space();
        match self.bytes.get(self.offset).copied() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected token")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn literal(&mut self, literal: &str, value: Json) -> Result<Json, MinamiError> {
        if !self.bytes[self.offset..].starts_with(literal.as_bytes()) {
            return Err(self.error("invalid literal"));
        }
        self.offset += literal.len();
        Ok(value)
    }

    fn object(&mut self, depth: usize) -> Result<Json, MinamiError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.depth_limit());
        }
        self.offset += 1;
        let mut object = BTreeMap::new();
        self.skip_space();
        if self.bytes.get(self.offset) == Some(&b'}') {
            self.offset += 1;
            return Ok(Json::Object(object));
        }
        loop {
            self.skip_space();
            if self.bytes.get(self.offset) != Some(&b'"') {
                return Err(self.error("object key must be a string"));
            }
            self.entry_budget()?;
            let key = self.string()?;
            self.skip_space();
            if self.bytes.get(self.offset) != Some(&b':') {
                return Err(self.error("expected ':' after object key"));
            }
            self.offset += 1;
            let value = self.value(depth)?;
            if object.insert(key, value).is_some() {
                return Err(self.error("duplicate object key"));
            }
            self.skip_space();
            match self.bytes.get(self.offset) {
                Some(b',') => self.offset += 1,
                Some(b'}') => {
                    self.offset += 1;
                    return Ok(Json::Object(object));
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, MinamiError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.depth_limit());
        }
        self.offset += 1;
        let mut array = Vec::new();
        self.skip_space();
        if self.bytes.get(self.offset) == Some(&b']') {
            self.offset += 1;
            return Ok(Json::Array(array));
        }
        loop {
            self.entry_budget()?;
            array.push(self.value(depth)?);
            self.skip_space();
            match self.bytes.get(self.offset) {
                Some(b',') => self.offset += 1,
                Some(b']') => {
                    self.offset += 1;
                    return Ok(Json::Array(array));
                }
                _ => return Err(self.error("expected ',' or ']' in array")),
            }
        }
    }

    fn string(&mut self) -> Result<String, MinamiError> {
        self.offset += 1;
        let mut output = String::new();
        let mut start = self.offset;
        loop {
            let Some(&byte) = self.bytes.get(self.offset) else {
                return Err(self.error("unterminated string"));
            };
            match byte {
                b'"' => {
                    output.push_str(&self.text[start..self.offset]);
                    self.offset += 1;
                    return Ok(output);
                }
                b'\\' => {
                    output.push_str(&self.text[start..self.offset]);
                    self.offset += 1;
                    self.escape(&mut output)?;
                    start = self.offset;
                }
                0x00..=0x1f => {
                    return Err(self.error("unescaped control character in string"));
                }
                _ => self.offset += 1,
            }
        }
    }

    fn escape(&mut self, output: &mut String) -> Result<(), MinamiError> {
        let Some(&escaped) = self.bytes.get(self.offset) else {
            return Err(self.error("unterminated string escape"));
        };
        self.offset += 1;
        match escaped {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => {
                let first = self.hex4()?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if !self.bytes[self.offset..].starts_with(b"\\u") {
                        return Err(self.error("unpaired high surrogate"));
                    }
                    self.offset += 2;
                    let second = self.hex4()?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(self.error("invalid low surrogate"));
                    }
                    0x10000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(self.error("unpaired low surrogate"));
                } else {
                    first as u32
                };
                output.push(
                    char::from_u32(scalar)
                        .ok_or_else(|| self.error("invalid Unicode scalar value"))?,
                );
            }
            _ => return Err(self.error("invalid string escape")),
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u16, MinamiError> {
        let end = self.offset + 4;
        let Some(digits) = self.bytes.get(self.offset..end) else {
            return Err(self.error("truncated Unicode escape"));
        };
        let mut value = 0u16;
        for &byte in digits {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error("invalid Unicode escape")),
            };
            value = (value << 4) | u16::from(digit);
        }
        self.offset = end;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, MinamiError> {
        let start = self.offset;
        if self.bytes.get(self.offset) == Some(&b'-') {
            self.offset += 1;
        }
        match self.bytes.get(self.offset).copied() {
            Some(b'0') => {
                self.offset += 1;
                if matches!(self.bytes.get(self.offset), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zero in number"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.bytes.get(self.offset), Some(b'0'..=b'9')) {
                    self.offset += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.bytes.get(self.offset) == Some(&b'.') {
            self.offset += 1;
            let digits = self.offset;
            while matches!(self.bytes.get(self.offset), Some(b'0'..=b'9')) {
                self.offset += 1;
            }
            if self.offset == digits {
                return Err(self.error("missing fraction digits"));
            }
        }
        if matches!(self.bytes.get(self.offset), Some(b'e' | b'E')) {
            self.offset += 1;
            if matches!(self.bytes.get(self.offset), Some(b'+' | b'-')) {
                self.offset += 1;
            }
            let digits = self.offset;
            while matches!(self.bytes.get(self.offset), Some(b'0'..=b'9')) {
                self.offset += 1;
            }
            if self.offset == digits {
                return Err(self.error("missing exponent digits"));
            }
        }
        Ok(Json::Number(self.text[start..self.offset].to_owned()))
    }
}

fn invalid(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::InvalidReference {
        resource: resource.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn budgeted(bytes: &[u8], values: usize, entries: usize) -> Result<Json, MinamiError> {
        parse_json_with_budget(bytes, "test", ParseBudget::new(values, entries))
    }

    fn budget_error(result: Result<Json, MinamiError>) -> &'static str {
        match result {
            Err(MinamiError::LimitExceeded { limit, .. }) => limit,
            Err(other) => panic!("expected budget error, got {other}"),
            Ok(_) => panic!("expected budget error, got accepted JSON"),
        }
    }

    #[test]
    fn p01_exact_json_budget_is_accepted() {
        assert_eq!(
            budgeted(br#"[[],[]]"#, 3, 2).unwrap(),
            Json::Array(vec![Json::Array(Vec::new()), Json::Array(Vec::new())])
        );
    }

    #[test]
    fn p02_first_value_or_entry_over_budget_is_rejected() {
        assert_eq!(
            budget_error(budgeted(b"[null,null]", 2, 2)),
            "JSON value budget"
        );
        assert_eq!(
            budget_error(budgeted(br#"[[],[],[]]"#, 3, 2)),
            "JSON entry budget"
        );
    }

    #[test]
    fn p03_many_small_values_stop_at_the_value_budget() {
        let json = format!("[{}]", "null,".repeat(9_999) + "null");
        assert_eq!(
            budget_error(budgeted(json.as_bytes(), 100, 20_000)),
            "JSON value budget"
        );
    }

    #[test]
    fn p04_many_small_objects_stop_at_the_entry_budget() {
        let json = format!("[{}]", vec!["{}"; 5_000].join(","));
        assert_eq!(
            budget_error(budgeted(json.as_bytes(), 20_000, 100)),
            "JSON entry budget"
        );
    }

    #[test]
    fn p05_object_keys_consume_the_entry_budget() {
        let json = format!(
            "{{{}}}",
            (0..5_000)
                .map(|index| format!("\"k{index}\":null"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            budget_error(budgeted(json.as_bytes(), 20_000, 100)),
            "JSON entry budget"
        );
        assert!(budgeted(br#"{"a":null,"b":null}"#, 3, 2).is_ok());
        assert_eq!(
            budget_error(budgeted(br#"{"a":null,"b":null}"#, 3, 1)),
            "JSON entry budget"
        );
    }

    #[test]
    fn p06_nested_structures_stop_at_the_value_budget() {
        let json = "[".repeat(32) + "0" + &"]".repeat(32);
        assert_eq!(
            budget_error(budgeted(json.as_bytes(), 10, 100)),
            "JSON value budget"
        );
    }

    #[test]
    fn p07_depth_is_checked_independently_of_the_budget() {
        let json = "[".repeat(MAX_JSON_DEPTH + 1) + "0" + &"]".repeat(MAX_JSON_DEPTH + 1);
        assert_eq!(
            budget_error(budgeted(json.as_bytes(), 10_000, 10_000)),
            "64 JSON nesting levels"
        );
    }

    #[test]
    fn p08_budget_counters_use_checked_arithmetic() {
        assert_eq!(budget_error(budgeted(b"null", 0, 0)), "JSON value budget");
        assert!(budgeted(b"null", usize::MAX, usize::MAX).is_ok());

        let mut extreme = ParseBudget::new(usize::MAX, usize::MAX);
        assert!(extreme.take_value());
        assert!(extreme.take_entry());
        let mut one = ParseBudget::new(1, 1);
        assert!(one.take_value());
        assert!(!one.take_value());
        assert!(one.take_entry());
        assert!(!one.take_entry());
    }

    #[test]
    fn p09_p10_budget_exhaustion_stops_before_a_malformed_tail() {
        let malformed = br#"[null,null,!]"#;
        assert!(matches!(
            parse_json(malformed, "test"),
            Err(MinamiError::InvalidReference { .. })
        ));
        assert_eq!(
            budget_error(budgeted(malformed, 2, 16)),
            "JSON value budget"
        );
    }

    #[test]
    fn v09_file_size_budget_reads_one_byte_past_without_truncating() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.json");
        fs::write(&path, b"123456789").unwrap();
        assert!(matches!(
            read_limited(&path, 8, "test budget"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        assert_eq!(read_limited(&path, 9, "test budget").unwrap(), b"123456789");
    }

    #[test]
    fn v64_json_depth_duplicate_keys_surrogates_and_byte_encoding_are_bounded() {
        let shallow = "[".repeat(MAX_JSON_DEPTH) + "0" + &"]".repeat(MAX_JSON_DEPTH);
        assert!(parse_json(shallow.as_bytes(), "test").is_ok());
        let deep = "[".repeat(MAX_JSON_DEPTH + 1) + "0" + &"]".repeat(MAX_JSON_DEPTH + 1);
        assert!(matches!(
            parse_json(deep.as_bytes(), "test"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let deep_empty = "[".repeat(MAX_JSON_DEPTH) + "[]" + &"]".repeat(MAX_JSON_DEPTH);
        assert!(matches!(
            parse_json(deep_empty.as_bytes(), "test"),
            Err(MinamiError::LimitExceeded { .. })
        ));

        assert!(parse_json(br#"{"a":1,"a":2}"#, "test").is_err());
        assert!(parse_json(br#"{"a":{"b":1,"b":2}}"#, "test").is_err());
        assert!(parse_json(br#""\ud800""#, "test").is_err());
        assert!(parse_json(br#""\udc00""#, "test").is_err());
        assert!(parse_json(r#""\u000€""#.as_bytes(), "test").is_err());
        assert!(parse_json(r#""\u12""#.as_bytes(), "test").is_err());
        assert_eq!(
            parse_json(br#""\ud83d\ude00""#, "test").unwrap(),
            Json::String("😀".into())
        );

        let first = parse_json(br#"{"b":{"d":3,"c":2},"a":1}"#, "test").unwrap();
        let second = parse_json(br#"{"a":1,"b":{"c":2,"d":3}}"#, "test").unwrap();
        assert_eq!(first, second);

        let mut bom = vec![0xef, 0xbb, 0xbf];
        bom.extend_from_slice(br#"{}"#);
        assert!(parse_json(&bom, "test").is_err());
        assert!(parse_json(&[b'{', 0xff, b'}'], "test").is_err());
        assert!(parse_json(br#"{} {}"#, "test").is_err());
    }

    #[test]
    fn v64_response_count_part_text_and_file_budgets_are_enforced() {
        let mut totals = Totals {
            segments: MAX_SEGMENTS,
            ..Totals::default()
        };
        assert!(matches!(
            totals.add_segment("test"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let mut totals = Totals {
            parts: MAX_PARTS,
            ..Totals::default()
        };
        assert!(matches!(
            totals.add_part("test"),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let mut totals = Totals {
            text_bytes: MAX_TEXT_BYTES,
            ..Totals::default()
        };
        assert!(matches!(
            totals.add_text(1, "test"),
            Err(MinamiError::LimitExceeded { .. })
        ));

        let too_many = vec![PathBuf::new(); MAX_RESPONSE_FILES + 1];
        assert!(matches!(
            validate_epub(
                Path::new("missing.epub"),
                Path::new("control.json"),
                &too_many,
                None
            ),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let two = vec![PathBuf::from("a.json"), PathBuf::from("b.json")];
        assert!(matches!(
            validate_epub(
                Path::new("missing.epub"),
                Path::new("control.json"),
                &two,
                Some(Path::new("batch.json"))
            ),
            Err(MinamiError::InvalidReference { .. })
        ));
    }
}
