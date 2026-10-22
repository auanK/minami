use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use serde::Serialize;

use crate::{MinamiError, x};

pub(crate) const MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;
const MAX_NUMBER: usize = 999_999;
const ITEM_SEPARATOR_BYTES: usize = 2;

#[derive(Debug)]
pub(crate) struct BatchPlan {
    pub file_name: String,
    pub section_id: String,
    pub segments: Vec<usize>,
    pub bytes: usize,
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub batches: Vec<BatchPlan>,
    pub max_bytes: usize,
}

#[derive(Serialize)]
struct BatchPayload<'a> {
    format_version: &'static str,
    extraction_id: &'a str,
    segments: Vec<&'a x::SourceSegment>,
}

fn invalid(resource: &str, detail: impl Into<String>) -> MinamiError {
    MinamiError::InvalidReference {
        resource: resource.into(),
        detail: detail.into(),
    }
}

fn serialization_error(error: serde_json::Error) -> MinamiError {
    MinamiError::Io {
        operation: "serializing X batch",
        resource: "X JSON".into(),
        source: io::Error::other(error),
    }
}

pub(crate) fn validate_limit(limit: usize) -> Result<(), MinamiError> {
    if limit == 0 {
        return Err(invalid("batch limit", "must be a positive byte count"));
    }
    if limit > MAX_BATCH_BYTES {
        return Err(MinamiError::LimitExceeded {
            resource: "batch limit".into(),
            limit: "64 MiB serialized X artifact",
        });
    }
    Ok(())
}

fn section_number(section_id: &str) -> Result<usize, MinamiError> {
    let digits = section_id.strip_prefix('q').unwrap_or_default();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(
            "batch section",
            format!("section ID '{section_id}' is not qN"),
        ));
    }
    let number = digits.parse::<usize>().map_err(|_| {
        invalid(
            "batch section",
            format!("section number '{section_id}' is too large"),
        )
    })?;
    if number == 0 || number > MAX_NUMBER {
        return Err(invalid(
            "batch section",
            format!("section number '{section_id}' exceeds six-digit naming capacity"),
        ));
    }
    Ok(number)
}

fn envelope_prefix(extraction_id: &str) -> Result<String, MinamiError> {
    let quoted_id = serde_json::to_string(extraction_id).map_err(serialization_error)?;
    Ok(format!(
        "{{\n  \"format_version\": \"{}\",\n  \"extraction_id\": {},\n  \"segments\": [\n",
        x::VERSION,
        quoted_id
    ))
}

fn segment_cost(segment: &x::SourceSegment) -> Result<usize, MinamiError> {
    let json = serde_json::to_vec_pretty(segment).map_err(serialization_error)?;
    let lines = 1 + json.iter().filter(|&&byte| byte == b'\n').count();
    json.len()
        .checked_add(4 * lines)
        .ok_or_else(|| invalid("batch plan", "segment serialized size overflow"))
}

fn encode_batch(payload: &BatchPayload<'_>) -> Result<Vec<u8>, MinamiError> {
    let mut bytes = serde_json::to_vec_pretty(payload).map_err(serialization_error)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(crate) fn plan(
    control: &x::Control,
    source: &x::Payload,
    max_bytes: usize,
) -> Result<Plan, MinamiError> {
    validate_limit(max_bytes)?;
    if control.format_version != x::VERSION || source.format_version != x::VERSION {
        return Err(invalid("batch plan", "X format_version is not 1.0"));
    }
    if control.extraction_id != source.extraction_id {
        return Err(invalid(
            "batch plan",
            "control and source extraction_id differ",
        ));
    }
    if control.segments.len() != source.segments.len() {
        return Err(invalid(
            "batch plan",
            "control and source segment counts differ",
        ));
    }

    let mut section_indices = BTreeMap::new();
    for (index, section) in control.sections.iter().enumerate() {
        if section_indices.insert(section.id.as_str(), index).is_some() {
            return Err(invalid(
                "batch plan",
                format!("duplicate section ID '{}'", section.id),
            ));
        }
    }

    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); control.sections.len()];
    for (index, segment) in control.segments.iter().enumerate() {
        let source_segment = source.segments.get(index).ok_or_else(|| {
            invalid(
                "batch plan",
                format!("source segment {} is missing", segment.id),
            )
        })?;
        if segment.id != source_segment.id {
            return Err(invalid(
                "batch plan",
                format!(
                    "control segment '{}' and source segment '{}' differ at index {index}",
                    segment.id, source_segment.id
                ),
            ));
        }
        let section_index = section_indices
            .get(segment.section_id.as_str())
            .copied()
            .ok_or_else(|| {
                invalid(
                    "batch plan",
                    format!(
                        "segment '{}' names unknown section '{}'",
                        segment.id, segment.section_id
                    ),
                )
            })?;
        groups[section_index].push(index);
    }

    let prefix = envelope_prefix(&control.extraction_id)?;
    let base = prefix.len() + "\n  ]\n}\n".len();
    if max_bytes < base {
        return Err(invalid(
            "batch limit",
            format!("{max_bytes} bytes is smaller than the X envelope overhead ({base} bytes)"),
        ));
    }

    let mut batches = Vec::new();
    let mut file_names = BTreeSet::new();
    for (section_index, section) in control.sections.iter().enumerate() {
        let indices = &groups[section_index];
        if indices.is_empty() {
            continue;
        }
        let number = section_number(&section.id)?;
        let mut batch_number = 1usize;
        let mut current = Vec::new();
        let mut current_bytes = base;

        for &index in indices {
            let cost = segment_cost(&source.segments[index])?;
            let needed = if current.is_empty() {
                base + cost
            } else {
                current_bytes + ITEM_SEPARATOR_BYTES + cost
            };
            if needed <= max_bytes {
                current.push(index);
                current_bytes = needed;
                continue;
            }
            if current.is_empty() {
                return Err(segment_too_large(
                    &source.segments[index],
                    section,
                    needed,
                    max_bytes,
                ));
            }
            push_batch(
                &mut batches,
                &mut file_names,
                number,
                batch_number,
                &section.id,
                std::mem::take(&mut current),
                current_bytes,
            )?;
            batch_number += 1;
            let needed = base + cost;
            if needed > max_bytes {
                return Err(segment_too_large(
                    &source.segments[index],
                    section,
                    needed,
                    max_bytes,
                ));
            }
            current.push(index);
            current_bytes = needed;
        }

        if !current.is_empty() {
            push_batch(
                &mut batches,
                &mut file_names,
                number,
                batch_number,
                &section.id,
                current,
                current_bytes,
            )?;
        }
    }

    let plan = Plan { batches, max_bytes };
    verify(control, source, &plan)?;
    Ok(plan)
}

fn push_batch(
    batches: &mut Vec<BatchPlan>,
    file_names: &mut BTreeSet<String>,
    section_number: usize,
    batch_number: usize,
    section_id: &str,
    segments: Vec<usize>,
    bytes: usize,
) -> Result<(), MinamiError> {
    if batch_number > MAX_NUMBER {
        return Err(invalid(
            "batch plan",
            format!("section {section_id} exceeds six-digit batch naming capacity"),
        ));
    }
    let file_name = format!("q{section_number:06}-{batch_number:06}.json");
    if !file_names.insert(file_name.clone()) {
        return Err(invalid(
            "batch plan",
            format!("duplicate batch file name '{file_name}'"),
        ));
    }
    batches.push(BatchPlan {
        file_name,
        section_id: section_id.into(),
        segments,
        bytes,
    });
    Ok(())
}

fn segment_too_large(
    segment: &x::SourceSegment,
    section: &x::Section,
    needed: usize,
    max_bytes: usize,
) -> MinamiError {
    invalid(
        "batch plan",
        format!(
            "segment {} in section {} needs at least {} bytes, exceeding --batch-max-bytes {}",
            segment.id, section.id, needed, max_bytes
        ),
    )
}

pub(crate) fn verify(
    control: &x::Control,
    source: &x::Payload,
    plan: &Plan,
) -> Result<(), MinamiError> {
    validate_limit(plan.max_bytes)?;
    if control.format_version != x::VERSION
        || source.format_version != x::VERSION
        || control.extraction_id != source.extraction_id
    {
        return Err(invalid(
            "batch plan",
            "X identity or format_version is inconsistent",
        ));
    }
    if control.segments.len() != source.segments.len() {
        return Err(invalid(
            "batch plan",
            "control and source segment counts differ",
        ));
    }
    let section_ids: BTreeSet<&str> = control
        .sections
        .iter()
        .map(|section| section.id.as_str())
        .collect();
    if section_ids.len() != control.sections.len() {
        return Err(invalid("batch plan", "duplicate section ID"));
    }

    let mut expected_index = 0usize;
    let mut segment_ids = BTreeSet::new();
    let mut file_names = BTreeSet::new();
    for batch in &plan.batches {
        if batch.segments.is_empty() {
            return Err(invalid(
                "batch plan",
                format!("batch '{}' is empty", batch.file_name),
            ));
        }
        if !file_names.insert(batch.file_name.as_str()) {
            return Err(invalid(
                "batch plan",
                format!("duplicate batch file name '{}'", batch.file_name),
            ));
        }
        if !section_ids.contains(batch.section_id.as_str()) {
            return Err(invalid(
                "batch plan",
                format!("batch '{}' has unknown section", batch.file_name),
            ));
        }
        if batch.bytes == 0 || batch.bytes > plan.max_bytes {
            return Err(invalid(
                "batch plan",
                format!(
                    "batch '{}' size {} exceeds --batch-max-bytes {}",
                    batch.file_name, batch.bytes, plan.max_bytes
                ),
            ));
        }
        for &index in &batch.segments {
            if index != expected_index {
                return Err(invalid(
                    "batch plan",
                    format!(
                        "segment coverage/order mismatch at position {expected_index}: found index {index}"
                    ),
                ));
            }
            expected_index += 1;
            let control_segment = control
                .segments
                .get(index)
                .ok_or_else(|| invalid("batch plan", format!("unknown segment index {index}")))?;
            let source_segment = source.segments.get(index).ok_or_else(|| {
                invalid(
                    "batch plan",
                    format!("unknown source segment index {index}"),
                )
            })?;
            if control_segment.id != source_segment.id {
                return Err(invalid(
                    "batch plan",
                    format!("segment identity differs at index {index}"),
                ));
            }
            if !segment_ids.insert(control_segment.id.as_str()) {
                return Err(invalid(
                    "batch plan",
                    format!("duplicate segment ID '{}'", control_segment.id),
                ));
            }
            if control_segment.section_id != batch.section_id {
                return Err(invalid(
                    "batch plan",
                    format!(
                        "segment '{}' belongs to section '{}' but batch '{}' is for '{}'",
                        control_segment.id,
                        control_segment.section_id,
                        batch.file_name,
                        batch.section_id
                    ),
                ));
            }
        }
    }
    if expected_index != control.segments.len() {
        return Err(invalid(
            "batch plan",
            format!(
                "segment coverage ends at {expected_index}, expected {}",
                control.segments.len()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn serialize_batch(
    source: &x::Payload,
    batch: &BatchPlan,
) -> Result<Vec<u8>, MinamiError> {
    let segments = batch
        .segments
        .iter()
        .map(|&index| {
            source.segments.get(index).ok_or_else(|| {
                invalid(
                    "batch plan",
                    format!(
                        "batch '{}' references unknown segment index {index}",
                        batch.file_name
                    ),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = encode_batch(&BatchPayload {
        format_version: x::VERSION,
        extraction_id: &source.extraction_id,
        segments,
    })?;
    if bytes.len() != batch.bytes {
        return Err(invalid(
            "batch plan",
            format!(
                "batch '{}' planned {} bytes but serialized to {}",
                batch.file_name,
                batch.bytes,
                bytes.len()
            ),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::extract::{Extraction, extract_data};
    use crate::application::support::*;
    use serde_json::Value;

    fn wrap(body: &str) -> String {
        format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
    }

    fn nav(targets: &[&str]) -> String {
        let points = targets
            .iter()
            .enumerate()
            .map(|(i, target)| {
                format!(
                    "<navPoint id='n{i}'><navLabel><text>Section {i}</text></navLabel><content src='{target}'/></navPoint>"
                )
            })
            .collect::<String>();
        format!(
            "<ncx xmlns='http://www.daisy.org/z3986/2005/ncx/' version='2005-1'><navMap>{points}</navMap></ncx>"
        )
    }

    fn book_entries(body: &str, targets: &[&str]) -> Vec<Entry> {
        let mut data = entries();
        replace(&mut data, "OEBPS/Text/Cover.xhtml", wrap(""));
        replace(&mut data, "OEBPS/Text/Illustrations.xhtml", wrap(""));
        replace(&mut data, "OEBPS/Text/Body.xhtml", wrap(body));
        replace(&mut data, "OEBPS/toc.ncx", nav(targets));
        data
    }

    fn book(body: &str) -> Fixture {
        Fixture::new(book_entries(body, &["Text/Body.xhtml"]))
    }

    fn data(body: &str) -> Extraction {
        let fixture = book(body);
        extract_data(&fixture.path).unwrap()
    }

    fn planned(data: &Extraction, max_bytes: usize) -> Plan {
        plan(&data.control, &data.source, max_bytes).unwrap()
    }

    fn encoded(data: &Extraction, plan: &Plan) -> Vec<Vec<u8>> {
        plan.batches
            .iter()
            .map(|batch| serialize_batch(&data.source, batch).unwrap())
            .collect()
    }

    fn flattened_ids(data: &Extraction, plan: &Plan) -> Vec<String> {
        plan.batches
            .iter()
            .flat_map(|batch| batch.segments.iter())
            .map(|&index| data.source.segments[index].id.clone())
            .collect()
    }

    fn flattened_values(data: &Extraction, plan: &Plan) -> Vec<Value> {
        encoded(data, plan)
            .into_iter()
            .flat_map(|bytes| {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                value["segments"].as_array().unwrap().clone()
            })
            .collect()
    }

    #[test]
    fn b01_one_section_fits_in_one_batch() {
        let data = data("<p>One.</p><p>Two.</p>");
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0].section_id, "q3");
        assert_eq!(plan.batches[0].file_name, "q000003-000001.json");
        assert_eq!(plan.batches[0].segments.len(), 2);
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b02_one_section_splits_in_source_order() {
        let data = data(
            &(1..=10)
                .map(|i| format!("<p>Paragraph {i}.</p>"))
                .collect::<String>(),
        );
        let full = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(full.batches.len(), 1);
        let limit = full.batches[0].bytes / 3;
        let split = planned(&data, limit);
        assert!(split.batches.len() > 1);
        assert!(split.batches.iter().all(|b| b.bytes <= limit));
        assert_eq!(flattened_ids(&data, &split), flattened_ids(&data, &full));
        verify(&data.control, &data.source, &split).unwrap();
    }

    #[test]
    fn b03_different_sections_never_share_a_batch() {
        let fixture = Fixture::new(book_entries(
            "<h1 id='one'>One</h1><p>Alpha</p><h1 id='two'>Two</h1><p>Beta</p>",
            &["Text/Body.xhtml#one", "Text/Body.xhtml#two"],
        ));
        let data = extract_data(&fixture.path).unwrap();
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(plan.batches[0].section_id, "q3");
        assert_eq!(plan.batches[1].section_id, "q4");
        assert!(plan.batches.iter().all(|b| b.section_id != "q1"));
        assert!(plan.batches.iter().all(|b| b.section_id != "q2"));
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b04_segment_parts_stay_together_at_the_limit() {
        let data = data("<p>Before <i>important</i><br/> after.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(full.batches.len(), 1);
        let limit = full.batches[0].bytes;
        let exact = planned(&data, limit);
        assert_eq!(exact.batches.len(), 1);
        assert_eq!(exact.batches[0].segments.len(), 1);
        let parts = &data.source.segments[0].parts;
        assert!(parts.len() > 2);
        let value: Value =
            serde_json::from_slice(&serialize_batch(&data.source, &exact.batches[0]).unwrap())
                .unwrap();
        assert_eq!(
            value["segments"][0]["parts"].as_array().unwrap().len(),
            parts.len()
        );
    }

    #[test]
    fn b05_oversized_segment_fails_with_context() {
        let data = data("<p>This segment is deliberately too large for the tiny budget.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        let limit = full.batches[0].bytes - 1;
        let error = plan(&data.control, &data.source, limit)
            .unwrap_err()
            .to_string();
        assert!(error.contains("s1"), "{error}");
        assert!(error.contains("q3"), "{error}");
        assert!(error.contains(&limit.to_string()), "{error}");
    }

    #[test]
    fn b06_empty_section_has_no_batch() {
        let data = data("<p>Only body text.</p>");
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(plan.batches.len(), 1);
        assert!(plan.batches.iter().all(|b| b.section_id == "q3"));
        assert!(plan.batches.iter().all(|b| !b.segments.is_empty()));
    }

    #[test]
    fn b07_no_segments_produces_no_batches() {
        let data = data("");
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert!(plan.batches.is_empty());
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b08_segment_ids_remain_the_global_extraction_ids() {
        let data = data("<p>One.</p><p>Two.</p><p>Three.</p>");
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(flattened_ids(&data, &plan), ["s1", "s2", "s3"]);
        let value: Value = serde_json::from_slice(&encoded(&data, &plan)[0]).unwrap();
        assert_eq!(value["segments"][0]["id"], "s1");
        assert_eq!(value["segments"][2]["id"], "s3");
    }

    #[test]
    fn b09_order_is_source_order_not_lexicographic() {
        let data = data(&(1..=100).map(|i| format!("<p>{i}</p>")).collect::<String>());
        let plan = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(plan.batches.len(), 1);
        let ids = flattened_ids(&data, &plan);
        let position = |id: &str| ids.iter().position(|candidate| candidate == id).unwrap();
        assert!(position("s2") < position("s9"));
        assert!(position("s9") < position("s10"));
        assert!(position("s10") < position("s100"));
        assert_eq!(
            ids,
            data.source
                .segments
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn b10_exact_serialized_size_is_accepted() {
        let data = data("<p>One.</p><p>Two.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        let exact = full.batches[0].bytes;
        let plan = planned(&data, exact);
        assert_eq!(plan.batches.len(), 1);
        let bytes = serialize_batch(&data.source, &plan.batches[0]).unwrap();
        assert_eq!(bytes.len(), exact);
        assert!(bytes.ends_with(b"\n"));
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b11_one_byte_over_splits_when_possible() {
        let data = data("<p>One.</p><p>Two.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        let limit = full.batches[0].bytes - 1;
        let plan = planned(&data, limit);
        assert!(plan.batches.len() > 1);
        for batch in &plan.batches {
            let bytes = serialize_batch(&data.source, batch).unwrap();
            assert!(bytes.len() <= limit);
            assert_eq!(bytes.len(), batch.bytes);
        }
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b12_json_escapes_are_counted_as_published() {
        let data = data("<p>Quote \" backslash \\ tab\tline\nend.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(full.batches.len(), 1);
        let bytes = serialize_batch(&data.source, &full.batches[0]).unwrap();
        assert_eq!(bytes.len(), full.batches[0].bytes);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r#"\""#), "{text}");
        assert!(text.contains(r#"\\"#), "{text}");
        assert!(text.contains(r#"\t"#), "{text}");
        assert!(text.contains(r#"\n"#), "{text}");
    }

    #[test]
    fn b13_unicode_is_measured_in_utf8_bytes() {
        let data = data("<p>Japanese 日本語, accent café, emoji 😀.</p>");
        let full = planned(&data, MAX_BATCH_BYTES);
        assert_eq!(full.batches.len(), 1);
        let bytes = serialize_batch(&data.source, &full.batches[0]).unwrap();
        let raw_text = match &data.source.segments[0].parts[0] {
            x::Part::Text(text) => text,
            _ => panic!("first part should be text"),
        };
        assert!(bytes.len() > raw_text.len());
        assert_eq!(bytes.len(), full.batches[0].bytes);
    }

    #[test]
    fn b14_envelope_overhead_is_part_of_the_budget() {
        let data = data("<p>Envelope.</p>");
        let standalone = serde_json::to_vec_pretty(&data.source.segments[0]).unwrap();
        let full = planned(&data, MAX_BATCH_BYTES);
        assert!(full.batches[0].bytes > standalone.len());
        assert!(plan(&data.control, &data.source, standalone.len()).is_err());
    }

    #[test]
    fn b15_invalid_limits_are_rejected() {
        let data = data("<p>Limit.</p>");
        assert!(validate_limit(0).is_err());
        assert!(validate_limit(MAX_BATCH_BYTES + 1).is_err());
        assert!(plan(&data.control, &data.source, 1).is_err());
    }

    #[test]
    fn b16_batch_union_equals_the_monolithic_payload() {
        let fixture = Fixture::new(book_entries(
            "<h1 id='one'>One</h1><p>Alpha</p><h1 id='two'>Two</h1><p>Beta</p>",
            &["Text/Body.xhtml#one", "Text/Body.xhtml#two"],
        ));
        let data = extract_data(&fixture.path).unwrap();
        let plan = planned(&data, 700);
        assert!(plan.batches.len() > 1);
        let expected = serde_json::to_value(&data.source).unwrap();
        let expected_segments = expected["segments"].as_array().unwrap();
        assert_eq!(&flattened_values(&data, &plan), expected_segments);
        for bytes in encoded(&data, &plan) {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 3);
            assert_eq!(value["format_version"], x::VERSION);
            assert_eq!(value["extraction_id"], data.source.extraction_id);
            assert!(value.get("section_id").is_none());
            assert!(value.get("batch_id").is_none());
        }
        verify(&data.control, &data.source, &plan).unwrap();
    }

    #[test]
    fn b17_duplicate_segment_in_plan_is_rejected() {
        let data = data("<p>One.</p><p>Two.</p>");
        let mut plan = planned(&data, MAX_BATCH_BYTES);
        let duplicate = plan.batches[0].segments[0];
        plan.batches[0].segments.push(duplicate);
        assert!(verify(&data.control, &data.source, &plan).is_err());
    }

    #[test]
    fn b18_omitted_segment_in_plan_is_rejected() {
        let data = data("<p>One.</p><p>Two.</p>");
        let mut plan = planned(&data, MAX_BATCH_BYTES);
        plan.batches[0].segments.remove(0);
        assert!(verify(&data.control, &data.source, &plan).is_err());
    }

    #[test]
    fn b19_segment_moved_to_another_section_is_rejected() {
        let fixture = Fixture::new(book_entries(
            "<h1 id='one'>One</h1><p>Alpha</p><h1 id='two'>Two</h1><p>Beta</p>",
            &["Text/Body.xhtml#one", "Text/Body.xhtml#two"],
        ));
        let data = extract_data(&fixture.path).unwrap();
        let mut plan = planned(&data, MAX_BATCH_BYTES);
        let first_section = plan.batches[0].section_id.clone();
        let second = plan
            .batches
            .iter()
            .position(|batch| batch.section_id != first_section)
            .unwrap();
        let moved = plan.batches[second].segments.remove(0);
        plan.batches[0].segments.push(moved);
        assert!(verify(&data.control, &data.source, &plan).is_err());
    }

    #[test]
    fn b20_same_input_produces_same_names_and_bytes() {
        let fixture = book("<p>One.</p><p>Two.</p><p>Three.</p>");
        let a = extract_data(&fixture.path).unwrap();
        let b = extract_data(&fixture.path).unwrap();
        let pa = planned(&a, 500);
        let pb = planned(&b, 500);
        assert!(!pa.batches.is_empty());
        assert_eq!(
            pa.batches.iter().map(|b| &b.file_name).collect::<Vec<_>>(),
            pb.batches.iter().map(|b| &b.file_name).collect::<Vec<_>>()
        );
        assert_eq!(encoded(&a, &pa), encoded(&b, &pb));
    }

    #[test]
    fn b21_budget_changes_grouping_but_not_identity_or_segments() {
        let data = data(
            &(1..=10)
                .map(|i| format!("<p>Paragraph {i}.</p>"))
                .collect::<String>(),
        );
        let before = serde_json::to_value(&data.control).unwrap();
        let full = planned(&data, MAX_BATCH_BYTES);
        let limit = full.batches[0].bytes / 2;
        let split = planned(&data, limit);
        assert_ne!(full.batches.len(), split.batches.len());
        assert_eq!(serde_json::to_value(&data.control).unwrap(), before);
        assert_eq!(flattened_ids(&data, &full), flattened_ids(&data, &split));
        assert_eq!(
            flattened_ids(&data, &split),
            data.source
                .segments
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        );
        verify(&data.control, &data.source, &full).unwrap();
        verify(&data.control, &data.source, &split).unwrap();
    }
}
