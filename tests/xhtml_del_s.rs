#[allow(dead_code)]
mod support;

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::*;
use zip::ZipArchive;

const BODY: &str = "OEBPS/Text/Body.xhtml";

fn wrap(body: &str) -> String {
    format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
}

fn book(body: &str) -> Fixture {
    let mut files = entries();
    replace(&mut files, BODY, wrap(body));
    replace(&mut files, "OEBPS/Text/Cover.xhtml", wrap(""));
    replace(&mut files, "OEBPS/Text/Illustrations.xhtml", wrap(""));
    replace(
        &mut files,
        "OEBPS/toc.ncx",
        NCX.replace("#one", "")
            .replace("#two", "")
            .replace("#three", ""),
    );
    Fixture::new(files)
}

fn extract(epub: &Path, output: &Path, batch: Option<usize>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_minami"));
    command.arg("extract").arg(epub).arg("--output").arg(output);
    if let Some(limit) = batch {
        command.arg("--batch-max-bytes").arg(limit.to_string());
    }
    command.output().unwrap()
}

fn validate(epub: &Path, control: &Path, translations: &[PathBuf], batch: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_minami"));
    command
        .arg("validate")
        .arg(epub)
        .arg("--control")
        .arg(control);
    for path in translations {
        command.arg("--translation").arg(path);
    }
    if let Some(batch) = batch {
        command.arg("--expected-batch").arg(batch);
    }
    command.output().unwrap()
}

fn build(epub: &Path, control: &Path, translation: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("build")
        .arg(epub)
        .arg("--control")
        .arg(control)
        .arg("--translation")
        .arg(translation)
        .arg("--output")
        .arg(output)
        .output()
        .unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn success(result: &Output) {
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn entries_by_name(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (entry.name().unwrap().to_string(), bytes)
        })
        .collect()
}

fn set_text(parts: &mut [Value], index: usize, value: &str) {
    parts[index]["text"] = json!(value);
}

#[test]
fn extraction_accepts_del_and_s_as_inline_markers_with_original_spans() {
    let body = "<p>Before <del class='strike'>removed <i>nested <b>bold <span>deep</span></b></i> A&#160;B</del> between <s style='text-decoration:line-through'>second</s> after <del>third</del><s></s>!</p>";
    let epub = book(body);
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("work");
    let result = extract(&epub.path, &output, None);
    success(&result);

    let control = read_json(&output.join("control.json"));
    let source = read_json(&output.join("source.json"));
    assert!(control.get("profile").is_none());
    assert_eq!(control["format_version"], "1.0");
    assert_eq!(
        source["segments"][0]["parts"],
        json!([
            {"text":"Before "},
            {"open":"del1"},
            {"text":"removed "},
            {"open":"i2"},
            {"text":"nested "},
            {"open":"b3"},
            {"text":"bold "},
            {"open":"span4"},
            {"text":"deep"},
            {"close":"span4"},
            {"close":"b3"},
            {"close":"i2"},
            {"text":" A"},
            {"atom":"nbsp5"},
            {"text":"B"},
            {"close":"del1"},
            {"text":" between "},
            {"open":"s6"},
            {"text":"second"},
            {"close":"s6"},
            {"text":" after "},
            {"open":"del7"},
            {"text":"third"},
            {"close":"del7"},
            {"atom":"s8"},
            {"text":"!"}
        ])
    );

    let html = wrap(body);
    for (id, expected_open, expected_close) in [
        ("del1", "<del class='strike'>", "</del>"),
        ("s6", "<s style='text-decoration:line-through'>", "</s>"),
    ] {
        let marker = control["segments"][0]["markers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|marker| marker["id"] == id)
            .unwrap();
        let open_range = marker["open"].as_array().unwrap();
        let close_range = marker["close"].as_array().unwrap();
        assert_eq!(
            &html[open_range[0].as_u64().unwrap() as usize
                ..open_range[1].as_u64().unwrap() as usize],
            expected_open
        );
        assert_eq!(
            &html[close_range[0].as_u64().unwrap() as usize
                ..close_range[1].as_u64().unwrap() as usize],
            expected_close
        );
    }
}

#[test]
fn validation_and_reconstruction_keep_del_s_tags_attributes_and_resources() {
    let body = "<p class='p'><del class='strike' data-k='keep'>gone</del> after</p><p><s style='text-decoration:line-through'>old</s> visible</p>";
    let epub = book(body);
    let original = fs::read(&epub.path).unwrap();
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    success(&extract(&epub.path, &work, None));
    let control_path = work.join("control.json");
    let mut response = read_json(&work.join("source.json"));

    let identity_response = root.path().join("identity.json");
    write_json(&identity_response, &response);
    let identity_output = root.path().join("identity.epub");
    success(&build(
        &epub.path,
        &control_path,
        &identity_response,
        &identity_output,
    ));
    let original_resources = entries_by_name(&epub.path);
    let identity_resources = entries_by_name(&identity_output);
    for (name, bytes) in &original_resources {
        assert_eq!(identity_resources[name], *bytes, "{name}");
    }

    let first = response["segments"][0]["parts"].as_array_mut().unwrap();
    *first = vec![
        json!({"text":"antes "}),
        json!({"text":"depois "}),
        json!({"open":"del1"}),
        json!({"text":"riscado"}),
        json!({"close":"del1"}),
    ];
    let second = response["segments"][1]["parts"].as_array_mut().unwrap();
    set_text(second, 1, "antigo");
    set_text(second, 3, " claro");
    let translated = root.path().join("translated.json");
    write_json(&translated, &response);
    let validation = validate(
        &epub.path,
        &control_path,
        std::slice::from_ref(&translated),
        None,
    );
    success(&validation);

    let translated_output = root.path().join("translated.epub");
    success(&build(
        &epub.path,
        &control_path,
        &translated,
        &translated_output,
    ));
    let rebuilt = entries_by_name(&translated_output);
    let rebuilt_body = String::from_utf8(rebuilt[BODY].clone()).unwrap();
    assert!(
        rebuilt_body.contains(
            "<p class='p'>antes depois <del class='strike' data-k='keep'>riscado</del></p>"
        )
    );
    assert!(
        rebuilt_body.contains("<p><s style='text-decoration:line-through'>antigo</s> claro</p>")
    );
    for (name, bytes) in &original_resources {
        if name != BODY {
            assert_eq!(rebuilt[name], *bytes, "{name}");
        }
    }
    assert_eq!(fs::read(&epub.path).unwrap(), original);
}

#[test]
fn batches_with_del_s_are_deterministic_and_validate_as_partial_or_complete() {
    let body = format!(
        "<p><del>{}</del></p><p><s>{}</s></p><p><del>{}</del></p>",
        "a".repeat(11_000),
        "b".repeat(11_000),
        "c".repeat(11_000)
    );
    let epub = book(&body);
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("batches-one");
    let second = root.path().join("batches-two");
    success(&extract(&epub.path, &first, Some(32768)));
    success(&extract(&epub.path, &second, Some(32768)));
    assert_eq!(
        fs::read(first.join("control.json")).unwrap(),
        fs::read(second.join("control.json")).unwrap()
    );

    let read_batches = |directory: &Path| {
        let mut paths: Vec<_> = fs::read_dir(directory.join("batches"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        paths
    };
    let batches = read_batches(&first);
    let other_batches = read_batches(&second);
    assert!(batches.len() > 1);
    assert_eq!(batches.len(), other_batches.len());
    for (left, right) in batches.iter().zip(&other_batches) {
        assert_eq!(left.file_name(), right.file_name());
        assert_eq!(fs::read(left).unwrap(), fs::read(right).unwrap());
        assert!(fs::metadata(left).unwrap().len() <= 32768);
        let partial = validate(
            &epub.path,
            &first.join("control.json"),
            std::slice::from_ref(left),
            Some(left),
        );
        success(&partial);
        assert!(String::from_utf8_lossy(&partial.stdout).contains("selected batch"));
    }
    let complete = validate(&epub.path, &first.join("control.json"), &batches, None);
    success(&complete);
    assert!(String::from_utf8_lossy(&complete.stdout).contains("complete extraction"));
}

#[test]
fn extraction_identity_is_deterministic_and_control_rejects_unknown_fields() {
    let plain = book("<p>Stable extraction identity.</p>");
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    success(&extract(&plain.path, &first, None));
    success(&extract(&plain.path, &second, None));
    let default_result = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("extract")
        .arg(&plain.path)
        .arg("--output")
        .arg(root.path().join("default"))
        .output()
        .unwrap();
    success(&default_result);
    let control = read_json(&first.join("control.json"));
    assert_eq!(
        fs::read(first.join("control.json")).unwrap(),
        fs::read(second.join("control.json")).unwrap()
    );
    assert_eq!(
        fs::read(first.join("source.json")).unwrap(),
        fs::read(second.join("source.json")).unwrap()
    );
    let digest = Sha256::digest(fs::read(&plain.path).unwrap());
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let expected = Sha256::digest(format!("minami-x\n1.0\n{digest}\n"));
    let expected: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(control.get("profile").is_none());
    assert_eq!(control["format_version"], "1.0");
    assert_eq!(control["extraction_id"], expected);

    let mut old_control = control;
    old_control["profile"] = json!("old");
    let old_control_path = root.path().join("old-control.json");
    write_json(&old_control_path, &old_control);
    let rejected_control = validate(
        &plain.path,
        &old_control_path,
        &[first.join("source.json")],
        None,
    );
    assert!(!rejected_control.status.success());
    assert!(String::from_utf8_lossy(&rejected_control.stderr).contains("does not match"));

    let unsupported = book("<p><ruby>Unknown tag</ruby></p>");
    let destination = root.path().join("unsupported");
    let result = extract(&unsupported.path, &destination, None);
    assert!(!result.status.success());
    assert!(!destination.exists());
}

#[test]
fn response_markers_remain_strict_and_depth_bounded_with_del() {
    let epub = book("<p><del>A <i>B</i></del> C</p>");
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    success(&extract(&epub.path, &work, None));
    let control = work.join("control.json");
    let source = read_json(&work.join("source.json"));
    let validate_response = |name: &str, response: &Value| {
        let path = root.path().join(name);
        write_json(&path, response);
        validate(&epub.path, &control, std::slice::from_ref(&path), None)
    };

    let mut missing = source.clone();
    missing["segments"][0]["parts"] = json!([{"text":"A B C"}]);
    assert!(!validate_response("missing.json", &missing).status.success());

    let mut duplicate = source.clone();
    duplicate["segments"][0]["parts"]
        .as_array_mut()
        .unwrap()
        .insert(1, json!({"open":"del1"}));
    assert!(
        !validate_response("duplicate.json", &duplicate)
            .status
            .success()
    );

    let mut renamed = source.clone();
    renamed["segments"][0]["parts"][0]["open"] = json!("del99");
    renamed["segments"][0]["parts"][4]["close"] = json!("del99");
    assert!(!validate_response("renamed.json", &renamed).status.success());

    let mut bad_nesting = source.clone();
    bad_nesting["segments"][0]["parts"] = json!([
        {"open":"del1"}, {"open":"i2"}, {"text":"A"},
        {"close":"del1"}, {"close":"i2"}, {"text":" C"}
    ]);
    assert!(
        !validate_response("bad-nesting.json", &bad_nesting)
            .status
            .success()
    );

    let mut empty_subtree = source.clone();
    empty_subtree["segments"][0]["parts"][1]["text"] = json!("   ");
    empty_subtree["segments"][0]["parts"][3]["text"] = json!("   ");
    assert!(
        !validate_response("empty-subtree.json", &empty_subtree)
            .status
            .success()
    );

    let allowed = format!("<p>{}text{}</p>", "<del>".repeat(64), "</del>".repeat(64));
    let allowed = book(&allowed);
    let allowed_output = root.path().join("allowed");
    success(&extract(&allowed.path, &allowed_output, None));

    let empty = format!(
        "<p><del>{}{}</del>visible</p>",
        "<del>".repeat(64),
        "</del>".repeat(64)
    );
    let empty = book(&empty);
    let empty_output = root.path().join("empty-inline");
    success(&extract(&empty.path, &empty_output, None));
    let empty_source = read_json(&empty_output.join("source.json"));
    assert_eq!(
        empty_source["segments"][0]["parts"][0],
        json!({"atom":"del1"})
    );

    let excessive = format!("<p>{}text{}</p>", "<del>".repeat(65), "</del>".repeat(65));
    let excessive = book(&excessive);
    let excessive_output = root.path().join("excessive");
    let result = extract(&excessive.path, &excessive_output, None);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("64 paired inline nesting levels"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
