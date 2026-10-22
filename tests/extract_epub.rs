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
