#[allow(dead_code)]
mod support;

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    sync::{Arc, Barrier},
    thread,
};
use support::*;

fn book(body: &str) -> Fixture {
    let mut e = entries();
    let wrap = |body: &str| {
        format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
    };
    replace(&mut e, "OEBPS/Text/Cover.xhtml", wrap(""));
    replace(&mut e, "OEBPS/Text/Illustrations.xhtml", wrap(""));
    replace(&mut e, "OEBPS/Text/Body.xhtml", wrap(body));
    replace(
        &mut e,
        "OEBPS/toc.ncx",
        NCX.replace("#one", "")
            .replace("#two", "")
            .replace("#three", ""),
    );
    Fixture::new(e)
}

fn extract(input: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("extract")
        .arg(input)
        .arg("--output")
        .arg(output)
        .output()
        .unwrap()
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn c01_success_publishes_exactly_two_complete_json_files() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    let f = book("<p>Hello <i>world</i>.</p>");
    let before = fs::read(&f.path).unwrap();
    let result = extract(&f.path, &dest);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    let mut files: Vec<_> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, ["control.json", "source.json"]);
    let control = json(&dest.join("control.json"));
    let source = json(&dest.join("source.json"));
    assert_eq!(control["extraction_id"], source["extraction_id"]);
    assert_eq!(source["segments"][0]["id"], "s1");
    assert_eq!(control["documents"].as_array().unwrap().len(), 3);
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn i07_document_without_eligible_text_is_a_valid_extraction() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    let f = book("");
    let result = extract(&f.path, &dest);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let control = json(&dest.join("control.json"));
    let source = json(&dest.join("source.json"));
    assert_eq!(control["documents"].as_array().unwrap().len(), 3);
    assert_eq!(control["segments"].as_array().unwrap().len(), 0);
    assert_eq!(source["segments"].as_array().unwrap().len(), 0);
}

#[test]
fn c02_invalid_epub_publishes_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let f = Fixture::bytes(b"broken ZIP");
    let dest = parent.path().join("work");
    let result = extract(&f.path, &dest);
    assert!(!result.status.success() && result.stdout.is_empty() && !result.stderr.is_empty());
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn c03_unsupported_or_malformed_xhtml_never_publishes() {
    for body in [
        "<p><ruby>Unsupported</ruby></p>",
        "<p>truncated",
        "<p xml:space='preserve'> X </p>",
    ] {
        let parent = tempfile::tempdir().unwrap();
        let f = book(body);
        let result = extract(&f.path, &parent.path().join("work"));
        assert!(!result.status.success() && result.stdout.is_empty());
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }
}

#[test]
fn c04_existing_directory_or_file_is_never_overwritten() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("keep"), b"owned by user").unwrap();
    let f = book("<p>Text</p>");
    assert!(!extract(&f.path, &dest).status.success());
    assert_eq!(fs::read(dest.join("keep")).unwrap(), b"owned by user");
    let file = parent.path().join("file");
    fs::write(&file, b"file data").unwrap();
    assert!(!extract(&f.path, &file).status.success());
    assert_eq!(fs::read(file).unwrap(), b"file data");
    let empty = parent.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(!extract(&f.path, &empty).status.success());
    assert_eq!(fs::read_dir(empty).unwrap().count(), 0);
}

#[test]
fn c05_bad_output_parent_reports_io_error_without_partial_output() {
    let parent = tempfile::tempdir().unwrap();
    let file = parent.path().join("file");
    fs::write(&file, b"keep").unwrap();
    let f = book("<p>Text</p>");
    let result = extract(&f.path, &file.join("work"));
    assert!(!result.status.success() && result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("file"));
    assert_eq!(fs::read(file).unwrap(), b"keep");
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn c06_concurrent_publication_has_one_winner_and_no_mixed_files() {
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("work");
    let a = book("<p>Alpha</p>");
    let b = book("<p>Beta</p>");
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [&a.path, &b.path]
        .into_iter()
        .map(|path| {
            let path = path.clone();
            let output = output.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                extract(&path, &output)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.status.success()).count(), 1);
    let winner = results.iter().position(|r| r.status.success()).unwrap();
    let control = json(&output.join("control.json"));
    let source = json(&output.join("source.json"));
    let digest: String =
        Sha256::digest(fs::read(if winner == 0 { &a.path } else { &b.path }).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
    assert_eq!(control["source"]["epub_sha256"], digest);
    assert_eq!(
        source["segments"][0]["parts"][0]["text"],
        if winner == 0 { "Alpha" } else { "Beta" }
    );
    assert_eq!(control["extraction_id"], source["extraction_id"]);
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn c07_cli_outputs_are_deterministic() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("a");
    let b = parent.path().join("b");
    let f = book("<p>Text</p>");
    assert!(extract(&f.path, &a).status.success());
    assert!(extract(&f.path, &b).status.success());
    for name in ["control.json", "source.json"] {
        assert_eq!(
            fs::read(a.join(name)).unwrap(),
            fs::read(b.join(name)).unwrap()
        );
    }
}

#[test]
fn c08_inspect_keeps_its_json_contract() {
    let f = Fixture::new(entries());
    let result = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(&f.path)
        .output()
        .unwrap();
    assert!(result.status.success() && result.stderr.is_empty());
    let expected = serde_json::to_value(minami::inspect_epub(&f.path).unwrap()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        expected
    );
}

fn extract_batched(input: &Path, output: &Path, limit: usize) -> Output {
    Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("extract")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--batch-max-bytes")
        .arg(limit.to_string())
        .output()
        .unwrap()
}

fn batch_paths(output: &Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(output.join("batches"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
}

fn batch_jsons(output: &Path) -> Vec<Value> {
    batch_paths(output)
        .into_iter()
        .map(|path| json(&path))
        .collect()
}

fn batch_segments(output: &Path) -> Vec<Value> {
    batch_jsons(output)
        .into_iter()
        .flat_map(|batch| batch["segments"].as_array().unwrap().clone())
        .collect()
}

fn assert_batch_only_layout(output: &Path) {
    let mut entries: Vec<_> = fs::read_dir(output)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, ["batches", "control.json"]);
    assert!(!output.join("source.json").exists());
}

#[test]
fn m13_c01_without_option_keeps_monolithic_layout() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    let f = book("<p>Monolithic.</p>");
    let result = extract(&f.path, &dest);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    let mut entries: Vec<_> = fs::read_dir(&dest)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, ["control.json", "source.json"]);
    assert!(!dest.join("batches").exists());
}

#[test]
fn m13_c02_batching_publishes_control_and_section_scoped_batches() {
    let parent = tempfile::tempdir().unwrap();
    let mono = parent.path().join("mono");
    let batched = parent.path().join("batched");
    let f = book("<p>One.</p><p>Two.</p><p>Three.</p>");
    assert!(extract(&f.path, &mono).status.success());
    let result = extract_batched(&f.path, &batched, 700);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    assert_batch_only_layout(&batched);
    let paths = batch_paths(&batched);
    assert!(!paths.is_empty());
    let control = json(&batched.join("control.json"));
    let sections: Vec<(String, String)> = control["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| {
            (
                segment["id"].as_str().unwrap().into(),
                segment["section_id"].as_str().unwrap().into(),
            )
        })
        .collect();
    for path in &paths {
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("q000003-") && name.ends_with(".json"),
            "{name}"
        );
        let bytes = fs::read(path).unwrap();
        assert!(bytes.len() <= 700, "{name} is {}", bytes.len());
        let batch = json(path);
        assert_eq!(batch.as_object().unwrap().len(), 3);
        assert_eq!(batch["format_version"], "1.0");
        assert_eq!(batch["extraction_id"], control["extraction_id"]);
        let mut batch_sections = std::collections::BTreeSet::new();
        for segment in batch["segments"].as_array().unwrap() {
            let id = segment["id"].as_str().unwrap();
            let section = sections
                .iter()
                .find(|(candidate, _)| candidate == id)
                .unwrap()
                .1
                .clone();
            batch_sections.insert(section);
        }
        assert_eq!(batch_sections.len(), 1);
    }
    assert_eq!(
        serde_json::to_value(batch_segments(&batched)).unwrap(),
        json(&mono.join("source.json"))["segments"]
    );
}

#[test]
fn m13_c03_oversized_segment_publishes_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    let f = book(&format!("<p>{}</p>", "long text ".repeat(200)));
    let result = extract_batched(&f.path, &dest, 512);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("s1") && stderr.contains("q3") && stderr.contains("512"),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn m13_c04_batched_mode_never_replaces_existing_destination() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("keep"), b"owned by user").unwrap();
    let f = book("<p>Text</p>");
    let result = extract_batched(&f.path, &dest, 32768);
    assert!(!result.status.success());
    assert_eq!(fs::read(dest.join("keep")).unwrap(), b"owned by user");
    assert!(!dest.join("batches").exists());
    assert!(!dest.join("control.json").exists());
}

#[test]
fn m13_c05_concurrent_batched_publication_has_one_winner_and_no_mixed_batches() {
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("work");
    let alpha = (1..=12)
        .map(|i| format!("<p>Alpha {i}.</p>"))
        .collect::<String>();
    let beta = (1..=12)
        .map(|i| format!("<p>Beta {i}.</p>"))
        .collect::<String>();
    let a = book(&alpha);
    let b = book(&beta);
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [&a.path, &b.path]
        .into_iter()
        .map(|path| {
            let path = path.clone();
            let output = output.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                extract_batched(&path, &output, 600)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.status.success())
            .count(),
        1
    );
    let winner = results
        .iter()
        .position(|result| result.status.success())
        .unwrap();
    assert_batch_only_layout(&output);
    let segments = batch_segments(&output);
    assert!(segments.len() > 1);
    let winner_name = if winner == 0 { "Alpha" } else { "Beta" };
    let loser_name = if winner == 0 { "Beta" } else { "Alpha" };
    for segment in segments {
        let text = segment["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains(winner_name), "{text}");
        assert!(!text.contains(loser_name), "{text}");
    }
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn m13_i07_batched_extraction_without_segments_has_an_empty_batches_directory() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("work");
    let f = book("");
    let result = extract_batched(&f.path, &dest, 32768);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_batch_only_layout(&dest);
    assert_eq!(fs::read_dir(dest.join("batches")).unwrap().count(), 0);
    let control = json(&dest.join("control.json"));
    assert_eq!(control["segments"].as_array().unwrap().len(), 0);
}

#[test]
fn m13_b20_batched_publication_is_byte_deterministic() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("a");
    let b = parent.path().join("b");
    let f = book("<p>One.</p><p>Two.</p><p>Three.</p>");
    assert!(extract_batched(&f.path, &a, 600).status.success());
    assert!(extract_batched(&f.path, &b, 600).status.success());
    assert_eq!(
        fs::read(a.join("control.json")).unwrap(),
        fs::read(b.join("control.json")).unwrap()
    );
    let a_paths = batch_paths(&a);
    let b_paths = batch_paths(&b);
    assert_eq!(
        a_paths
            .iter()
            .map(|path| path.file_name().unwrap().to_owned())
            .collect::<Vec<_>>(),
        b_paths
            .iter()
            .map(|path| path.file_name().unwrap().to_owned())
            .collect::<Vec<_>>()
    );
    for (a_path, b_path) in a_paths.iter().zip(&b_paths) {
        assert_eq!(fs::read(a_path).unwrap(), fs::read(b_path).unwrap());
    }
}

#[test]
fn m13_c08_batched_control_is_byte_identical_to_monolithic_control() {
    let parent = tempfile::tempdir().unwrap();
    let mono = parent.path().join("mono");
    let batched = parent.path().join("batched");
    let f = book("<p>One <i>two</i>.</p><p>Three.</p>");
    assert!(extract(&f.path, &mono).status.success());
    assert!(extract_batched(&f.path, &batched, 32768).status.success());
    assert_eq!(
        fs::read(mono.join("control.json")).unwrap(),
        fs::read(batched.join("control.json")).unwrap()
    );
    assert_eq!(
        serde_json::to_value(batch_segments(&batched)).unwrap(),
        json(&mono.join("source.json"))["segments"]
    );
    let source = json(&mono.join("source.json"));
    for batch in batch_jsons(&batched) {
        assert_eq!(batch["format_version"], source["format_version"]);
        assert_eq!(batch["extraction_id"], source["extraction_id"]);
    }
}

#[test]
fn m13_b15_invalid_cli_batch_limits_fail_without_output() {
    let parent = tempfile::tempdir().unwrap();
    let f = book("<p>Text</p>");
    for (i, value) in ["0", "-1", "not-a-number", "67108865"]
        .into_iter()
        .enumerate()
    {
        let dest = parent.path().join(format!("work-{i}"));
        let result = Command::new(env!("CARGO_BIN_EXE_minami"))
            .arg("extract")
            .arg(&f.path)
            .arg("--output")
            .arg(&dest)
            .arg("--batch-max-bytes")
            .arg(value)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{value}");
        assert!(!dest.exists(), "{value}");
    }
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}
