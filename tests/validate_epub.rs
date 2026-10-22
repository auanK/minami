#![allow(clippy::too_many_lines)]

#[allow(dead_code)]
mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::*;

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

fn book(body: &str, targets: &[&str]) -> Fixture {
    let mut entries = entries();
    replace(&mut entries, "OEBPS/Text/Cover.xhtml", wrap(""));
    replace(&mut entries, "OEBPS/Text/Illustrations.xhtml", wrap(""));
    replace(&mut entries, "OEBPS/Text/Body.xhtml", wrap(body));
    replace(&mut entries, "OEBPS/toc.ncx", ncx(targets));
    Fixture::new(entries)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn digest(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct Case {
    book: Fixture,
    root: tempfile::TempDir,
    work: PathBuf,
    source: Value,
    control: Value,
}

impl Case {
    fn new(body: &str, targets: &[&str]) -> Self {
        let book = book(body, targets);
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        minami::extract_epub(&book.path, &work).unwrap();
        let source = read_json(&work.join("source.json"));
        let control = read_json(&work.join("control.json"));
        Self {
            book,
            root,
            work,
            source,
            control,
        }
    }

    fn default() -> Self {
        Self::new(
            "<p>Prelude.</p><h1 id='chapter'>Chapter</h1><p>Hello <i>world</i>.</p><p>A<br/>B <img src='x.png'/> C.</p><p><span id='note1'>Important note</span> and <b>additional information</b>.</p><p>Intro <a id='chapter-anchor'></a> continues.</p><p>Read <a href='#note1' class='note'>the note</a>.</p>",
            &["Text/Body.xhtml#chapter"],
        )
    }

    fn control_path(&self) -> PathBuf {
        self.work.join("control.json")
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.root.path().join(name);
        write_json(&path, value);
        path
    }

    fn write_bytes(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn response(&self) -> PathBuf {
        self.write("response.json", &self.source)
    }

    fn validate(&self, translations: &[PathBuf], batch: Option<&Path>) -> Output {
        validate(&self.book.path, &self.control_path(), translations, batch)
    }
}

fn validate(epub: &Path, control: &Path, translations: &[PathBuf], batch: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_minami"));
    command
        .arg("validate")
        .arg(epub)
        .arg("--control")
        .arg(control);
    for translation in translations {
        command.arg("--translation").arg(translation);
    }
    if let Some(batch) = batch {
        command.arg("--expected-batch").arg(batch);
    }
    command.output().unwrap()
}

#[track_caller]
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[track_caller]
fn failure(output: &Output) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
}

fn envelope(case: &Case, segments: Vec<Value>) -> Value {
    json!({
        "format_version": "1.0",
        "extraction_id": case.control["extraction_id"],
        "segments": segments,
    })
}

fn segment(value: &Value, id: &str) -> Value {
    value["segments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|segment| segment["id"] == id)
        .unwrap()
        .clone()
}

fn source_with_parts(case: &Case, id: &str, parts: Value) -> Value {
    let mut response = case.source.clone();
    let segment = response["segments"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|segment| segment["id"] == id)
        .unwrap();
    segment["parts"] = parts;
    response
}

#[test]
fn v01_v10_v11_v20_v61_complete_reordered_and_split_responses_are_accepted() {
    let case = Case::default();
    let response = case.response();
    let output = case.validate(&[response], None);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("complete extraction"), "{stdout}");

    let mut reordered = case.source.clone();
    reordered["segments"].as_array_mut().unwrap().reverse();
    let reordered = case.write("reordered.json", &reordered);
    success(&case.validate(&[reordered], None));

    let segments = case.source["segments"].as_array().unwrap();
    let middle = segments.len() / 2;
    let first = envelope(&case, segments[..middle].to_vec());
    let second = envelope(&case, segments[middle..].to_vec());
    let a = case.write("part-a.json", &first);
    let b = case.write("part-b.json", &second);
    success(&case.validate(&[a.clone(), b.clone()], None));
    success(&case.validate(&[b, a], None));
}

#[test]
fn v02_v03_malformed_and_non_pure_json_are_rejected() {
    let case = Case::default();
    let valid = serde_json::to_string(&case.source).unwrap();
    let mut variants = vec![
        "{".to_string(),
        valid[..valid.len() - 1].to_string(),
        format!("{valid}{{}}"),
        format!("Aqui está sua tradução:\n```json\n{valid}\n```"),
        format!("{valid}\ntrailing"),
    ];
    let mut without_comma = valid.clone();
    let comma = without_comma.rfind(',').unwrap();
    without_comma.remove(comma);
    variants.push(without_comma);
    for (index, raw) in variants.into_iter().enumerate() {
        let path = case.write_bytes(&format!("malformed-{index}.json"), raw.as_bytes());
        failure(&case.validate(&[path], None));
    }
}

#[test]
fn v04_v05_v06_unknown_missing_and_wrong_typed_fields_are_rejected() {
    let case = Case::default();
    let mut envelope_unknown = case.source.clone();
    envelope_unknown["profile"] = json!("epub2-xhtml-v1");
    let mut segment_unknown = case.source.clone();
    segment_unknown["segments"][0]["range"] = json!([0, 1]);
    let mut part_unknown = case.source.clone();
    part_unknown["segments"][0]["parts"][0]["html"] = json!("<b>markup</b>");
    for (index, value) in [envelope_unknown, segment_unknown, part_unknown]
        .into_iter()
        .enumerate()
    {
        let path = case.write(&format!("unknown-{index}.json"), &value);
        failure(&case.validate(&[path], None));
    }

    let mut missing_envelope = case.source.clone();
    missing_envelope
        .as_object_mut()
        .unwrap()
        .remove("format_version");
    let mut missing_segment = case.source.clone();
    missing_segment["segments"][0]
        .as_object_mut()
        .unwrap()
        .remove("id");
    let mut missing_parts = case.source.clone();
    missing_parts["segments"][0]
        .as_object_mut()
        .unwrap()
        .remove("parts");
    let mut empty_part = case.source.clone();
    empty_part["segments"][0]["parts"][0] = json!({});
    for (index, value) in [missing_envelope, missing_segment, missing_parts, empty_part]
        .into_iter()
        .enumerate()
    {
        let path = case.write(&format!("missing-{index}.json"), &value);
        failure(&case.validate(&[path], None));
    }

    let mut wrong_version = case.source.clone();
    wrong_version["format_version"] = json!(1);
    let mut wrong_segments = case.source.clone();
    wrong_segments["segments"] = json!({});
    let mut wrong_id = case.source.clone();
    wrong_id["segments"][0]["id"] = json!(1);
    let mut wrong_parts = case.source.clone();
    wrong_parts["segments"][0]["parts"] = json!({});
    let mut wrong_text = case.source.clone();
    wrong_text["segments"][0]["parts"][0]["text"] = json!(1);
    let mut wrong_open = case.source.clone();
    wrong_open["segments"][0]["parts"][0] = json!({"open": 1});
    let mut both_part_fields = case.source.clone();
    both_part_fields["segments"][0]["parts"][0] = json!({"text": "A", "html": "<b>A</b>"});
    for (index, value) in [
        wrong_version,
        wrong_segments,
        wrong_id,
        wrong_parts,
        wrong_text,
        wrong_open,
        both_part_fields,
    ]
    .into_iter()
    .enumerate()
    {
        let path = case.write(&format!("type-{index}.json"), &value);
        failure(&case.validate(&[path], None));
    }
}

#[test]
fn v07_duplicate_keys_are_rejected_at_every_level() {
    let case = Case::default();
    let valid = serde_json::to_string(&case.source).unwrap();
    let envelope_duplicate = valid.replacen(
        "\"format_version\":\"1.0\"",
        "\"format_version\":\"1.0\",\"format_version\":\"1.0\"",
        1,
    );
    let segment_duplicate = valid.replacen("{\"id\":\"s1\"", "{\"id\":\"s1\",\"id\":\"s1\"", 1);
    let part_duplicate = valid.replacen("{\"text\":\"", "{\"text\":\"x\",\"text\":\"", 1);
    assert_ne!(valid, envelope_duplicate);
    assert_ne!(valid, segment_duplicate);
    assert_ne!(valid, part_duplicate);
    for (index, raw) in [envelope_duplicate, segment_duplicate, part_duplicate]
        .into_iter()
        .enumerate()
    {
        let path = case.write_bytes(&format!("duplicate-{index}.json"), raw.as_bytes());
        failure(&case.validate(&[path], None));
    }
}

#[test]
fn v08_bom_and_invalid_utf8_are_rejected() {
    let case = Case::default();
    let bytes = serde_json::to_vec(&case.source).unwrap();
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(&bytes);
    failure(&case.validate(&[case.write_bytes("bom.json", &bom)], None));
    let invalid = [b'{', b'"', 0xff, b'"', b':', b'"', b'x', b'"', b'}'];
    failure(&case.validate(&[case.write_bytes("invalid-utf8.json", &invalid)], None));
}

#[test]
fn v12_v13_v14_v21_missing_duplicate_unknown_and_partial_segments_are_rejected() {
    let case = Case::default();
    let mut missing = case.source.clone();
    missing["segments"].as_array_mut().unwrap().pop();
    let missing = case.write("missing.json", &missing);
    failure(&case.validate(&[missing], None));

    let mut duplicated = case.source.clone();
    let duplicate = duplicated["segments"][1].clone();
    duplicated["segments"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let duplicated = case.write("duplicated.json", &duplicated);
    failure(&case.validate(&[duplicated], None));

    let mut unknown = case.source.clone();
    unknown["segments"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "s999", "parts": [{"text": "Extra."}]}));
    let unknown = case.write("unknown.json", &unknown);
    failure(&case.validate(&[unknown], None));

    let partial = envelope(&case, vec![segment(&case.source, "s1")]);
    let partial = case.write("partial.json", &partial);
    failure(&case.validate(&[partial], None));
}

#[test]
fn v15_v16_wrong_identity_or_version_is_rejected() {
    let case = Case::default();
    let mut wrong_id = case.source.clone();
    wrong_id["extraction_id"] = json!("0".repeat(64));
    let wrong_id = case.write("wrong-id.json", &wrong_id);
    failure(&case.validate(&[wrong_id], None));

    let mut wrong_version = case.source.clone();
    wrong_version["format_version"] = json!("2.0");
    let wrong_version = case.write("wrong-version.json", &wrong_version);
    failure(&case.validate(&[wrong_version], None));
}

#[test]
fn v17_altered_control_is_rejected_even_with_the_same_epub_digest() {
    let case = Case::default();
    let mut control = case.control.clone();
    control["segments"][0]["range"][0] =
        json!(control["segments"][0]["range"][0].as_u64().unwrap() + 1);
    let tampered = case.write("tampered-control.json", &control);
    let response = case.response();
    let output = validate(&case.book.path, &tampered, &[response], None);
    failure(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("control"));
}

#[test]
fn v18_epub_from_another_origin_is_rejected() {
    let case = Case::default();
    let other = book("<p>Different source.</p>", &["Text/Body.xhtml"]);
    let response = case.response();
    let output = validate(&other.path, &case.control_path(), &[response], None);
    failure(&output);
}

#[test]
fn v19_duplicate_ids_across_files_are_rejected() {
    let case = Case::default();
    let segments = case.source["segments"].as_array().unwrap();
    let first = envelope(&case, segments[..segments.len() - 1].to_vec());
    let mut last = segments[segments.len() - 1..].to_vec();
    last.push(segments[0].clone());
    let second = envelope(&case, last);
    let a = case.write("duplicate-a.json", &first);
    let b = case.write("duplicate-b.json", &second);
    failure(&case.validate(&[a, b], None));
}

#[test]
fn v22_v23_v24_v62_isolated_batch_validation_is_explicit_and_checked() {
    let case = Case::default();
    let selected = envelope(
        &case,
        vec![segment(&case.source, "s1"), segment(&case.source, "s2")],
    );
    let batch = case.write("batch.json", &selected);
    let translation = case.write("batch-translation.json", &selected);
    let output = case.validate(&[translation], Some(&batch));
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not a complete translation"), "{stdout}");
    assert!(!stdout.contains("complete extraction"), "{stdout}");

    let only_one = envelope(&case, vec![segment(&case.source, "s1")]);
    let only_one = case.write("batch-one.json", &only_one);
    failure(&case.validate(&[only_one], Some(&batch)));

    let extra = case.write("batch-extra.json", &case.source);
    failure(&case.validate(&[extra], Some(&batch)));

    let mut tampered = selected.clone();
    tampered["segments"][0]["parts"][0]["text"] = json!("changed source");
    let tampered = case.write("batch-tampered.json", &tampered);
    let translation = case.write("batch-good.json", &selected);
    failure(&case.validate(&[translation], Some(&tampered)));

    let batch = case.write(
        "batch-one-only.json",
        &envelope(&case, vec![segment(&case.source, "s1")]),
    );
    let translation = case.write(
        "batch-good-one.json",
        &envelope(&case, vec![segment(&case.source, "s1")]),
    );
    success(&case.validate(&[translation], Some(&batch)));
}

#[test]
fn v25_v26_v27_v31_marker_multiplicity_and_foreign_ids_are_rejected() {
    let case = Case::default();
    success(&case.validate(&[case.response()], None));

    let missing = source_with_parts(&case, "s3", json!([{"text": "Hello world."}]));
    let missing = case.write("marker-missing.json", &missing);
    failure(&case.validate(&[missing], None));

    let duplicated = source_with_parts(
        &case,
        "s3",
        json!([
            {"text": "Hello "},
            {"open": "i1"},
            {"text": "world"},
            {"open": "i1"},
            {"text": "again"},
            {"close": "i1"},
            {"close": "i1"},
            {"text": "."}
        ]),
    );
    let duplicated = case.write("marker-duplicated.json", &duplicated);
    failure(&case.validate(&[duplicated], None));

    let foreign = source_with_parts(
        &case,
        "s3",
        json!([
            {"text": "Hello "},
            {"atom": "br1"},
            {"text": "world."}
        ]),
    );
    let foreign = case.write("marker-foreign.json", &foreign);
    failure(&case.validate(&[foreign], None));
}

#[test]
fn v28_v29_v30_x04_x12_x13_nesting_parents_and_missing_pairs_are_checked() {
    let case = Case::new(
        "<p><b>Very <i>small</i></b> room.</p>",
        &["Text/Body.xhtml"],
    );
    success(&case.validate(&[case.response()], None));

    let crossed = source_with_parts(
        &case,
        "s1",
        json!([
            {"open": "b1"},
            {"text": "Very "},
            {"open": "i2"},
            {"text": "small"},
            {"close": "b1"},
            {"close": "i2"},
            {"text": " room."}
        ]),
    );
    let crossed = case.write("crossed.json", &crossed);
    failure(&case.validate(&[crossed], None));

    let reparented = source_with_parts(
        &case,
        "s1",
        json!([
            {"open": "b1"},
            {"text": "Very"},
            {"close": "b1"},
            {"open": "i2"},
            {"text": "small"},
            {"close": "i2"},
            {"text": " room."}
        ]),
    );
    let reparented = case.write("reparented.json", &reparented);
    failure(&case.validate(&[reparented], None));

    let duplicate = source_with_parts(
        &case,
        "s1",
        json!([
            {"open": "b1"},
            {"text": "Very "},
            {"open": "i2"},
            {"text": "small"},
            {"close": "i2"},
            {"open": "i2"},
            {"text": "again"},
            {"close": "i2"},
            {"close": "b1"},
            {"text": " room."}
        ]),
    );
    let duplicate = case.write("nested-duplicate.json", &duplicate);
    failure(&case.validate(&[duplicate], None));

    let missing = source_with_parts(&case, "s1", json!([{"text": "Very small room."}]));
    let missing = case.write("nested-missing.json", &missing);
    failure(&case.validate(&[missing], None));
}

#[test]
fn v32_v33_atoms_cannot_be_omitted_or_duplicated() {
    let case = Case::default();
    let missing = source_with_parts(
        &case,
        "s4",
        json!([{"text": "A"}, {"text": "B "}, {"atom": "img2"}, {"text": " C."}]),
    );
    let missing = case.write("atom-missing.json", &missing);
    failure(&case.validate(&[missing], None));

    let duplicate = source_with_parts(
        &case,
        "s4",
        json!([
            {"text": "A"},
            {"atom": "br1"},
            {"text": "B "},
            {"atom": "img2"},
            {"atom": "br1"},
            {"text": " C."}
        ]),
    );
    let duplicate = case.write("atom-duplicate.json", &duplicate);
    failure(&case.validate(&[duplicate], None));
}

#[test]
fn v34_v36_x17_global_atom_order_is_immutable() {
    let case = Case::new(
        "<p><i>One<br/>two</i><img src='image.png'/> End.</p>",
        &["Text/Body.xhtml"],
    );
    success(&case.validate(&[case.response()], None));
    let swapped = source_with_parts(
        &case,
        "s1",
        json!([
            {"atom": "img3"},
            {"open": "i1"},
            {"text": "Um"},
            {"atom": "br2"},
            {"text": "dois"},
            {"close": "i1"},
            {"text": " Fim."}
        ]),
    );
    let swapped = case.write("atom-order.json", &swapped);
    failure(&case.validate(&[swapped], None));
}

#[test]
fn v35_v39_x03_unanchored_groups_may_move_without_changing_atoms() {
    let case = Case::new(
        "<p><i>Only then</i> did I understand.</p>",
        &["Text/Body.xhtml"],
    );
    let moved = source_with_parts(
        &case,
        "s1",
        json!([
            {"text": "Eu entendi "},
            {"open": "i1"},
            {"text": "só então"},
            {"close": "i1"},
            {"text": "."}
        ]),
    );
    let moved = case.write("x03.json", &moved);
    success(&case.validate(&[moved], None));

    let case = Case::new("<p><i>One</i> <b>Two</b>.</p>", &["Text/Body.xhtml"]);
    let reordered = source_with_parts(
        &case,
        "s1",
        json!([
            {"open": "b2"},
            {"text": "Two"},
            {"close": "b2"},
            {"text": " "},
            {"open": "i1"},
            {"text": "One"},
            {"close": "i1"},
            {"text": "."}
        ]),
    );
    let reordered = case.write("groups.json", &reordered);
    success(&case.validate(&[reordered], None));
}

#[test]
fn v37_v38_v42_x18_xhtml_id_anchors_are_protected_even_without_ncx() {
    let case = Case::default();
    assert!(
        case.control["navigation"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["fragment"] != "note1")
    );
    success(&case.validate(&[case.response()], None));
    let crossed = source_with_parts(
        &case,
        "s5",
        json!([
            {"open": "b2"},
            {"text": "Informação adicional"},
            {"close": "b2"},
            {"text": " e "},
            {"open": "span1"},
            {"text": "nota importante"},
            {"close": "span1"},
            {"text": "."}
        ]),
    );
    let crossed = case.write("anchor-crossed.json", &crossed);
    failure(&case.validate(&[crossed], None));

    let accepted = source_with_parts(
        &case,
        "s5",
        json!([
            {"text": "Aqui, "},
            {"open": "span1"},
            {"text": "nota importante"},
            {"close": "span1"},
            {"text": " e "},
            {"open": "b2"},
            {"text": "informação adicional"},
            {"close": "b2"},
            {"text": "."}
        ]),
    );
    let accepted = case.write("anchor-accepted.json", &accepted);
    success(&case.validate(&[accepted], None));
}

#[test]
fn v40_v41_x18_empty_anchors_are_atoms_and_cannot_disappear() {
    let case = Case::default();
    success(&case.validate(&[case.response()], None));
    let missing = source_with_parts(&case, "s6", json!([{"text": "Introdução continua aqui."}]));
    let missing = case.write("empty-anchor-missing.json", &missing);
    failure(&case.validate(&[missing], None));

    let duplicate = source_with_parts(
        &case,
        "s6",
        json!([
            {"text": "Introdução "},
            {"atom": "a1"},
            {"atom": "a1"},
            {"text": " continua aqui."}
        ]),
    );
    let duplicate = case.write("empty-anchor-duplicate.json", &duplicate);
    failure(&case.validate(&[duplicate], None));
}

#[test]
fn v43_v44_promoted_ncx_boundaries_keep_their_structural_position() {
    let case = Case::new(
        "<p> <i id='promo'>Start</i> text.</p>",
        &["Text/Body.xhtml#promo"],
    );
    success(&case.validate(&[case.response()], None));

    let moved = source_with_parts(
        &case,
        "s1",
        json!([
            {"text": "Start "},
            {"open": "i1"},
            {"text": "text."},
            {"close": "i1"}
        ]),
    );
    let moved = case.write("ncx-moved.json", &moved);
    failure(&case.validate(&[moved], None));
}

#[test]
fn v45_v46_v47_v48_x01_semantics_unicode_and_html_looking_text_are_not_judged() {
    let case = Case::new("<p>Hello.</p>", &["Text/Body.xhtml"]);
    success(&case.validate(&[case.response()], None));
    for (index, text) in [
        "Eu odeio você.",
        "日本語 café ☕",
        "Leia <script>alert(1)</script> & <b>isto</b> aqui.",
    ]
    .into_iter()
    .enumerate()
    {
        let response = source_with_parts(&case, "s1", json!([{"text": text}]));
        let path = case.write(&format!("text-{index}.json"), &response);
        success(&case.validate(&[path], None));
    }
}

#[test]
fn v49_v50_x16_x19_invalid_xml_and_protected_spaces_in_text_are_rejected() {
    let case = Case::new("<p>Hello.</p>", &["Text/Body.xhtml"]);
    for (index, text) in ["\u{0}".to_string(), "\u{1}".to_string()]
        .into_iter()
        .enumerate()
    {
        let response = source_with_parts(&case, "s1", json!([{"text": text}]));
        let path = case.write(&format!("xml-char-{index}.json"), &response);
        failure(&case.validate(&[path], None));
    }
    for (index, text) in ["Olá\u{a0}mundo", "Olá\u{2007}mundo", "Olá\u{202f}mundo"]
        .into_iter()
        .enumerate()
    {
        let response = source_with_parts(&case, "s1", json!([{"text": text}]));
        let path = case.write(&format!("protected-{index}.json"), &response);
        failure(&case.validate(&[path], None));
    }

    let spaces = Case::new("<p>A&#160;B&#x2007;C&#x202F;D</p>", &["Text/Body.xhtml"]);
    success(&spaces.validate(&[spaces.response()], None));
    let mut response = spaces.source.clone();
    let parts = response["segments"][0]["parts"].as_array_mut().unwrap();
    let text = parts.last_mut().unwrap();
    *text = json!({"text": "D\u{a0}hoje"});
    let response = spaces.write("space-added.json", &response);
    failure(&spaces.validate(&[response], None));
}

#[test]
fn v52_v53_x16_blank_segments_and_empty_paired_subtrees_are_rejected() {
    let case = Case::new("<p>Hello.</p>", &["Text/Body.xhtml"]);
    let blank = source_with_parts(&case, "s1", json!([{"text": " \t\r\n"}]));
    let blank = case.write("blank.json", &blank);
    failure(&case.validate(&[blank], None));

    let nested = Case::new("<p>Before <i>inside</i> after.</p>", &["Text/Body.xhtml"]);
    let empty = source_with_parts(
        &nested,
        "s1",
        json!([
            {"text": "Antes "},
            {"open": "i1"},
            {"text": " "},
            {"close": "i1"},
            {"text": " depois."}
        ]),
    );
    let empty = nested.write("empty-pair.json", &empty);
    failure(&nested.validate(&[empty], None));
}

#[test]
fn v54_large_translated_text_is_accepted_within_resource_limits() {
    let case = Case::new("<p>Short.</p>", &["Text/Body.xhtml"]);
    let response = source_with_parts(&case, "s1", json!([{"text": "x".repeat(100_000)}]));
    let response = case.write("large-text.json", &response);
    success(&case.validate(&[response], None));
}

#[test]
fn v55_v56_v57_validation_is_read_only_and_never_writes_an_epub() {
    let case = Case::default();
    let good = case.response();
    let bad = source_with_parts(&case, "s1", json!([{"text": "Prelude."}, {"id": "extra"}]));
    let bad = case.write("bad.json", &bad);
    let epub_before = digest(&case.book.path);
    let control_before = fs::read(case.control_path()).unwrap();
    let good_before = fs::read(&good).unwrap();
    let bad_before = fs::read(&bad).unwrap();
    let entries_before = fs::read_dir(case.root.path()).unwrap().count();
    failure(&case.validate(std::slice::from_ref(&bad), None));
    assert_eq!(digest(&case.book.path), epub_before);
    assert_eq!(fs::read(case.control_path()).unwrap(), control_before);
    assert_eq!(fs::read(&good).unwrap(), good_before);
    assert_eq!(fs::read(&bad).unwrap(), bad_before);
    assert_eq!(
        fs::read_dir(case.root.path()).unwrap().count(),
        entries_before
    );
    assert_eq!(
        fs::read_dir(&case.work)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        ["control.json".into(), "source.json".into()]
            .into_iter()
            .collect()
    );
}

#[test]
fn v58_v59_v60_existing_commands_still_work() {
    let book = book("<p>Hello <i>world</i>.</p>", &["Text/Body.xhtml"]);
    let root = tempfile::tempdir().unwrap();
    let mono = root.path().join("mono");
    let batched = root.path().join("batched");

    let inspect = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(&book.path)
        .output()
        .unwrap();
    success(&inspect);

    let extract = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("extract")
        .arg(&book.path)
        .arg("--output")
        .arg(&mono)
        .output()
        .unwrap();
    success(&extract);
    assert!(mono.join("control.json").is_file() && mono.join("source.json").is_file());

    let batched_output = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("extract")
        .arg(&book.path)
        .arg("--output")
        .arg(&batched)
        .arg("--batch-max-bytes")
        .arg("4096")
        .output()
        .unwrap();
    success(&batched_output);
    assert!(batched.join("control.json").is_file() && batched.join("batches").is_dir());
}

#[test]
fn v63_a_read_failure_stops_without_partial_success_output() {
    let case = Case::default();
    let segments = case.source["segments"].as_array().unwrap();
    let first = envelope(&case, vec![segments[0].clone()]);
    let first = case.write("first.json", &first);
    let missing = case.root.path().join("does-not-exist.json");
    let output = case.validate(&[first, missing], None);
    failure(&output);
}
