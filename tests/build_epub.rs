#[allow(dead_code)]
mod support;

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Barrier},
    thread,
};

use serde_json::{Value, json};
use support::*;
use zip::{CompressionMethod, ZipArchive};

const BODY: &str = "OEBPS/Text/Body.xhtml";

fn wrap(body: &str) -> String {
    format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
}

fn book_entries(body: &str) -> Vec<Entry> {
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
    files
}

struct Case {
    book: Fixture,
    root: tempfile::TempDir,
    work: PathBuf,
}

impl Case {
    fn new(files: Vec<Entry>) -> Self {
        Self::from_book(Fixture::new(files))
    }

    fn from_book(book: Fixture) -> Self {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        success(
            &Command::new(env!("CARGO_BIN_EXE_minami"))
                .arg("extract")
                .arg(&book.path)
                .arg("--output")
                .arg(&work)
                .output()
                .unwrap(),
        );
        Self { book, root, work }
    }

    fn body(body: &str) -> Self {
        Self::new(book_entries(body))
    }

    fn output(&self) -> PathBuf {
        self.root.path().join("translated.epub")
    }
    fn control(&self) -> PathBuf {
        self.work.join("control.json")
    }
    fn source(&self) -> PathBuf {
        self.work.join("source.json")
    }
    fn response(&self) -> Value {
        read_json(&self.source())
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.root.path().join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn build(&self, translations: &[PathBuf]) -> Output {
        build(
            &self.book.path,
            &self.control(),
            translations,
            &self.output(),
        )
    }
}

fn build(epub: &Path, control: &Path, translations: &[PathBuf], output: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_minami"));
    command.arg("build").arg(epub).arg("--control").arg(control);
    for path in translations {
        command.arg("--translation").arg(path);
    }
    command.arg("--output").arg(output).output().unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
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
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

fn resources(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|index| {
            let mut entry = zip.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (name, bytes)
        })
        .collect()
}

#[test]
fn bld01_bld02_cli_extract_build_inspect_with_modified_translation() {
    let case = Case::body("<p>Hello.</p>");
    let mut response = case.response();
    response["segments"][0]["parts"] = json!([{"text":"Olá."}]);
    let response = case.write("response.json", &response);
    let output = case.build(&[response]);
    success(&output);
    assert!(output.stdout.is_empty());
    let inspect = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(case.output())
        .output()
        .unwrap();
    success(&inspect);
    assert_eq!(
        resources(&case.output())[BODY],
        wrap("<p>Olá.</p>").as_bytes()
    );
    assert_eq!(
        minami::inspect_epub(&case.book.path).unwrap(),
        minami::inspect_epub(&case.output()).unwrap()
    );
    assert_eq!(fs::read_dir(case.root.path()).unwrap().count(), 3);
}

#[test]
fn bld13_mimetype_has_the_required_physical_header() {
    let case = Case::body("<p>Hello.</p>");
    success(&case.build(&[case.source()]));
    let mut zip = ZipArchive::new(File::open(case.output()).unwrap()).unwrap();
    let mime = zip.by_index(0).unwrap();
    assert_eq!(mime.name().unwrap(), "mimetype");
    assert_eq!(mime.header_start(), 0);
    assert_eq!(mime.data_start(), Some(38));
    assert_eq!(mime.compression(), CompressionMethod::Stored);
    drop(mime);
    assert_eq!(
        zip.file_names()
            .filter(|name| name.as_ref().unwrap() == "mimetype")
            .count(),
        1
    );
    let bytes = fs::read(case.output()).unwrap();
    assert_eq!(&bytes[28..30], &[0, 0]);
    assert_eq!(
        resources(&case.output())["mimetype"],
        b"application/epub+zip"
    );
}

#[test]
fn bld03_identity_preserves_protected_tokens_and_external_bytes() {
    let case = Case::body(
        "\n<p class='keep'>Caf&#233; <i title='kept'>world</i>&#160;☕<!--keep--><?keep value?></p>\n",
    );
    success(&case.build(&[case.source()]));
    let output = resources(&case.output());
    assert_eq!(output[BODY], wrap("\n<p class='keep'>Café <i title='kept'>world</i>&#160;☕<!--keep--><?keep value?></p>\n").as_bytes());
    let original = resources(&case.book.path);
    for (path, bytes) in &original {
        if path != BODY {
            assert_eq!(&output[path], bytes, "{path}");
        }
    }
    let again = case.root.path().join("again");
    minami::extract_epub(&case.output(), &again).unwrap();
    assert_eq!(
        read_json(&again.join("source.json"))["segments"],
        case.response()["segments"]
    );
}

#[test]
fn bld04_bld05_bld06_bld07_all_manifest_documents_survive() {
    let mut files = book_entries("<p>Body.</p>");
    replace(
        &mut files,
        "OEBPS/Text/Illustrations.xhtml",
        wrap("<p>Nonlinear.</p>"),
    );
    replace(&mut files, "OEBPS/content.opf", OPF.replace("</manifest>", "<item id='extra' href='Text/Extra.xhtml' media-type='application/xhtml+xml'/></manifest>"));
    files.push(entry(
        "OEBPS/Text/Extra.xhtml",
        wrap("<p>Outside spine.</p>"),
    ));
    let case = Case::new(files);
    let mut response = case.response();
    for segment in response["segments"].as_array_mut().unwrap() {
        for part in segment["parts"].as_array_mut().unwrap() {
            if let Some(text) = part["text"].as_str() {
                part["text"] = json!(format!("Traduzido {text}"));
            }
        }
    }
    let response = case.write("response.json", &response);
    minami::build_epub(
        &case.book.path,
        &case.control(),
        &[response],
        &case.output(),
    )
    .unwrap();
    let output = resources(&case.output());
    assert_eq!(output["OEBPS/Text/Cover.xhtml"], wrap("").as_bytes());
    for path in [
        BODY,
        "OEBPS/Text/Illustrations.xhtml",
        "OEBPS/Text/Extra.xhtml",
    ] {
        assert!(
            String::from_utf8_lossy(&output[path]).contains("Traduzido"),
            "{path}"
        );
    }
    assert!(!minami::inspect_epub(&case.output()).unwrap().spine[1].linear);
    assert_eq!(
        minami::inspect_epub(&case.output()).unwrap(),
        minami::inspect_epub(&case.book.path).unwrap()
    );
}

#[test]
fn bld08_sections_share_the_same_physical_xhtml() {
    let mut files = book_entries(
        "<h1 id='one'>One</h1><p>First.</p><h1 id='two'>Two</h1><p>Second.</p><h1 id='three'>Three</h1>",
    );
    replace(&mut files, "OEBPS/toc.ncx", NCX);
    let case = Case::new(files);
    success(&case.build(&[case.source()]));
    assert_eq!(resources(&case.book.path), resources(&case.output()));
    assert_eq!(
        minami::inspect_epub(&case.output())
            .unwrap()
            .navigation
            .len(),
        3
    );
}

#[test]
fn bld09_bld10_inline_reordering_uses_exact_original_tags() {
    let case =
        Case::body("<p>Before <i class='a'>One</i> <b title='b'>Two</b> <span>Three</span>.</p>");
    let mut response = case.response();
    response["segments"][0]["parts"] = json!([
        {"open":"b2"},{"text":"Dois"},{"close":"b2"},{"text":" "},
        {"open":"span3"},{"text":"Três"},{"close":"span3"},{"text":" "},
        {"open":"i1"},{"text":"Um"},{"close":"i1"},{"text":" depois."}
    ]);
    success(&case.build(&[case.write("response.json", &response)]));
    assert_eq!(
        resources(&case.output())[BODY],
        wrap("<p><b title='b'>Dois</b> <span>Três</span> <i class='a'>Um</i> depois.</p>")
            .as_bytes()
    );
}

#[test]
fn bld11_bld12_atoms_unicode_escaping_and_cr() {
    let case = Case::body(
        "<p>A<br/>B <img src='../Images/cover.png'/> C&#160;D<!--keep--><?keep value?>E</p>",
    );
    let mut response = case.response();
    for part in response["segments"][0]["parts"].as_array_mut().unwrap() {
        if part.get("text").is_some() {
            part["text"] = json!("日本語 😀 < & >\r");
        }
    }
    success(&case.build(&[case.write("response.json", &response)]));
    let output = resources(&case.output());
    let source = std::str::from_utf8(&output[BODY]).unwrap();
    for token in [
        "<br/>",
        "<img src='../Images/cover.png'/>",
        "&#160;",
        "<!--keep-->",
        "<?keep value?>",
        "日本語 😀 &lt; &amp; &gt;&#13;",
    ] {
        assert!(source.contains(token), "{token}");
    }
}

fn asset_entries() -> Vec<Entry> {
    let mut files = book_entries(
        "<p id='note'>Read <a href='#note'>here</a>.<img src='../Images/cover.png'/></p>",
    );
    files.push(entry("OEBPS/Styles/style.css", "p { color: #123; }\r\n"));
    files.push(entry(
        "OEBPS/Images/cover.png",
        [0x89, b'P', b'N', b'G', 0, 0xff, 0x80],
    ));
    files.push(entry("OEBPS/Fonts/font.otf", [0, 0xff, 0xf0, 0x80, 0x01]));
    files.push(entry("META-INF/auxiliary.bin", [0, 1, 2, 255]));
    files.push(entry(
        "OEBPS/Unlisted.xhtml",
        "unmanifested opaque bytes\0\u{ff}",
    ));
    files.push(entry("OEBPS/", ""));
    files.push(entry("OEBPS/Images/", ""));
    // The current reader accepts directory records with data; preserve that data too.
    files.push(entry("Auxiliary/", [0xff, 0, 1]));
    replace(&mut files, "OEBPS/content.opf", OPF.replace("</manifest>", "<item id='css' href='Styles/style.css' media-type='text/css'/><item id='image' href='Images/cover.png' media-type='image/png'/><item id='font' href='Fonts/font.otf' media-type='application/octet-stream'/></manifest>"));
    for (i, file) in files.iter_mut().enumerate().skip(1) {
        if i % 2 == 0 {
            file.compression = CompressionMethod::Deflated;
        }
    }
    files
}

#[test]
fn bld14_to_bld25_bld70_all_resources_paths_types_and_compression_survive() {
    let case = Case::new(asset_entries());
    success(&case.build(&[case.source()]));
    assert_eq!(resources(&case.book.path), resources(&case.output()));
    let mut original = ZipArchive::new(File::open(&case.book.path).unwrap()).unwrap();
    let mut rebuilt = ZipArchive::new(File::open(case.output()).unwrap()).unwrap();
    assert_eq!(original.len(), rebuilt.len());
    for i in 0..original.len() {
        let before = original.by_index(i).unwrap();
        let after = rebuilt.by_index(i).unwrap();
        assert_eq!(before.name().unwrap(), after.name().unwrap());
        assert_eq!(before.is_dir(), after.is_dir());
        assert_eq!(before.compression(), after.compression());
    }
}

fn rejected(case: &Case, response: &Value, diagnostic: &str) {
    let path = case.write("bad-response.json", response);
    let before = fs::read_dir(case.root.path()).unwrap().count();
    let result = case.build(&[path]);
    failure(&result);
    assert!(
        String::from_utf8_lossy(&result.stderr).contains(diagnostic),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!case.output().exists());
    assert_eq!(fs::read_dir(case.root.path()).unwrap().count(), before);
}

#[test]
fn bld26_control_mismatch_never_publishes() {
    let case = Case::body("<p>Hello.</p>");
    let mut control = read_json(&case.control());
    control["documents"][0]["path"] = json!("elsewhere.xhtml");
    fs::write(case.control(), serde_json::to_vec(&control).unwrap()).unwrap();
    failure(&case.build(&[case.source()]));
    assert!(!case.output().exists());
}

#[test]
fn bld27_to_bld32_invalid_identity_coverage_and_markers_never_publish() {
    let case = Case::body("<p>Hello <b>very <i>small</i></b>.</p><p>Second.</p>");
    let mut wrong_id = case.response();
    wrong_id["extraction_id"] = json!("0".repeat(64));
    let mut missing = case.response();
    missing["segments"].as_array_mut().unwrap().pop();
    let mut duplicate = case.response();
    let segment = duplicate["segments"][0].clone();
    duplicate["segments"].as_array_mut().unwrap().push(segment);
    let mut unknown = case.response();
    unknown["segments"][0]["id"] = json!("s999");
    let mut no_marker = case.response();
    no_marker["segments"][0]["parts"] = json!([{"text":"Words."}]);
    let mut nesting = case.response();
    nesting["segments"][0]["parts"] =
        json!([{"open":"b1"},{"open":"i2"},{"text":"Words"},{"close":"b1"},{"close":"i2"}]);
    for (response, diagnostic) in [
        (wrong_id, "extraction_id"),
        (missing, "missing"),
        (duplicate, "duplicate"),
        (unknown, "unknown"),
        (no_marker, "marker"),
        (nesting, "closes"),
    ] {
        rejected(&case, &response, diagnostic);
    }
}

#[test]
fn bld33_bld34_bld35_atom_order_anchors_and_protected_spaces_never_publish() {
    let case = Case::body("<p><span id='note'>One</span><br/>Two<img src='x.png'/>Three</p>");
    let mut atoms = case.response();
    let parts = atoms["segments"][0]["parts"].as_array_mut().unwrap();
    let positions: Vec<_> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.get("atom").map(|_| i))
        .collect();
    parts.swap(positions[0], positions[1]);
    rejected(&case, &atoms, "atom order");
    let mut anchor = case.response();
    anchor["segments"][0]["parts"] = json!([{"atom":"br2"},{"open":"span1"},{"text":"Um"},{"close":"span1"},{"atom":"img3"},{"text":"Fim"}]);
    rejected(&case, &anchor, "anchor");
    let mut space = case.response();
    space["segments"][0]["parts"][1]["text"] = json!("novo\u{a0}espaço");
    rejected(&case, &space, "protected space");
}

#[test]
fn bld36_linguistic_meaning_is_outside_validation() {
    let case = Case::body("<p>Hello.</p>");
    let mut response = case.response();
    response["segments"][0]["parts"] = json!([{"text":"Tradução sem relação com o original."}]);
    success(&case.build(&[case.write("response.json", &response)]));
}

#[test]
fn bld37_to_bld40_multiple_real_batches_require_complete_unique_coverage() {
    let case = Case::body("<p>One.</p><p>Two.</p><p>Three.</p><p>Four.</p>");
    let batched = case.root.path().join("batches-work");
    minami::extract_epub_batched(&case.book.path, &batched, 350).unwrap();
    let mut paths: Vec<_> = fs::read_dir(batched.join("batches"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert!(paths.len() > 1);
    failure(&case.build(&paths[..paths.len() - 1]));
    assert!(!case.output().exists());
    let mut duplicates = paths.clone();
    duplicates.push(paths[0].clone());
    failure(&case.build(&duplicates));
    assert!(!case.output().exists());
    success(&case.build(&paths));
    let first = fs::read(case.output()).unwrap();
    let reverse_output = case.root.path().join("reverse.epub");
    paths.reverse();
    success(&build(
        &case.book.path,
        &case.control(),
        &paths,
        &reverse_output,
    ));
    assert_eq!(first, fs::read(reverse_output).unwrap());
}

#[test]
fn bld45_bld57_last_auxiliary_resource_crc_failure_rolls_back() {
    let mut files = book_entries("<p>Hello.</p>");
    files.push(entry("last.bin", b"unique binary data for CRC failure"));
    let mut bytes = zip_bytes(files);
    let offset = bytes
        .windows(34)
        .position(|bytes| bytes == b"unique binary data for CRC failure")
        .unwrap();
    bytes[offset] ^= 1;
    let case = Case::from_book(Fixture::bytes(&bytes));
    let before = fs::read(&case.book.path).unwrap();
    let result = case.build(&[case.source()]);
    failure(&result);
    assert!(String::from_utf8_lossy(&result.stderr).contains("last.bin"));
    assert!(!case.output().exists());
    assert_eq!(fs::read(&case.book.path).unwrap(), before);
    assert_eq!(fs::read_dir(case.root.path()).unwrap().count(), 1);
}

#[test]
fn bld46_truncated_snapshot_is_rejected_without_output() {
    let case = Case::body("<p>Hello.</p>");
    let mut bytes = fs::read(&case.book.path).unwrap();
    bytes.truncate(bytes.len() - 10);
    fs::write(&case.book.path, bytes).unwrap();
    failure(&case.build(&[case.source()]));
    assert!(!case.output().exists());
}

#[test]
fn bld48_bld55_bld56_existing_destinations_and_all_inputs_are_preserved() {
    let case = Case::body("<p>Hello.</p>");
    let inputs = [&case.book.path, &case.control(), &case.source()];
    for path in inputs {
        let before = fs::read(path).unwrap();
        failure(&build(
            &case.book.path,
            &case.control(),
            &[case.source()],
            path,
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }
    for directory in [false, true] {
        let path = case.output();
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"user data").unwrap();
        }
        failure(&case.build(&[case.source()]));
        if directory {
            assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
            fs::remove_dir(path).unwrap();
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"user data");
            fs::remove_file(path).unwrap();
        }
    }
    let before: Vec<_> = inputs.iter().map(|path| fs::read(path).unwrap()).collect();
    success(&case.build(&[case.source()]));
    for (path, before) in inputs.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires Windows symlink privilege or Developer Mode"
)]
fn bld49_existing_symlinks_including_dangling_links_are_preserved() {
    let case = Case::body("<p>Hello.</p>");
    let target = case.root.path().join("target");
    fs::write(&target, b"user data").unwrap();
    let link = case.output();
    for dangling in [false, true] {
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_file(&target, &link);
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(&target, &link);
        result.expect("creating test symlink requires filesystem symlink support and permission");
        if dangling {
            fs::remove_file(&target).unwrap();
        }
        failure(&case.build(&[case.source()]));
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        if !dangling {
            assert_eq!(fs::read(&target).unwrap(), b"user data");
        }
        fs::remove_file(&link).unwrap();
    }
}

#[test]
fn bld50_concurrent_builds_publish_one_complete_winner() {
    let case = Case::body("<p>Hello.</p>");
    let mut a = case.response();
    a["segments"][0]["parts"] = json!([{"text":"Alpha"}]);
    let mut b = case.response();
    b["segments"][0]["parts"] = json!([{"text":"Beta"}]);
    let paths = [case.write("a.json", &a), case.write("b.json", &b)];
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = paths
        .into_iter()
        .map(|translation| {
            let (epub, control, output, barrier) = (
                case.book.path.clone(),
                case.control(),
                case.output(),
                Arc::clone(&barrier),
            );
            thread::spawn(move || {
                barrier.wait();
                build(&epub, &control, &[translation], &output)
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
    let winner = if results[0].status.success() {
        "Alpha"
    } else {
        "Beta"
    };
    assert_eq!(
        resources(&case.output())[BODY],
        wrap(&format!("<p>{winner}</p>")).as_bytes()
    );
    assert_eq!(fs::read_dir(case.root.path()).unwrap().count(), 4);
}

#[test]
fn bld58_missing_or_invalid_output_parent_is_not_created() {
    let case = Case::body("<p>Hello.</p>");
    let parent = case.root.path().join("missing");
    failure(&build(
        &case.book.path,
        &case.control(),
        &[case.source()],
        &parent.join("out.epub"),
    ));
    assert!(!parent.exists());
    fs::write(&parent, b"user data").unwrap();
    failure(&build(
        &case.book.path,
        &case.control(),
        &[case.source()],
        &parent.join("out.epub"),
    ));
    assert_eq!(fs::read(parent).unwrap(), b"user data");
}

#[test]
fn bld60_escaped_xhtml_over_the_limit_never_stages_output() {
    let case = Case::body("<p>Hello.</p>");
    let mut response = case.response();
    response["segments"][0]["parts"] = json!([{"text":"&".repeat(7*1024*1024)}]);
    rejected(&case, &response, "32 MiB");
}

#[test]
fn cli_requires_all_inputs_and_rejects_expected_batch() {
    for args in [
        vec!["build"],
        vec![
            "build",
            "book.epub",
            "--control",
            "control.json",
            "--output",
            "out.epub",
        ],
        vec![
            "build",
            "book.epub",
            "--translation",
            "response.json",
            "--output",
            "out.epub",
        ],
        vec![
            "build",
            "book.epub",
            "--control",
            "control.json",
            "--translation",
            "response.json",
        ],
        vec![
            "build",
            "book.epub",
            "--control",
            "control.json",
            "--translation",
            "response.json",
            "--output",
            "out.epub",
            "--expected-batch",
            "batch.json",
        ],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_minami"))
            .args(args)
            .output()
            .unwrap();
        failure(&result);
        assert_eq!(result.status.code(), Some(2));
    }
}

#[test]
fn bld66_zip64_local_entry_is_rejected_even_with_a_classic_end_record() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut files = book_entries("<p>Hello.</p>");
    files.push(entry("zip64.bin", [1, 2, 3]));
    for file in files {
        writer
            .start_file(
                &file.name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .large_file(file.name == "zip64.bin"),
            )
            .unwrap();
        writer.write_all(&file.data).unwrap();
    }
    let book = Fixture::bytes(&writer.finish().unwrap().into_inner());
    assert!(
        matches!(
            minami::inspect_epub(&book.path),
            Err(minami::MinamiError::Unsupported { .. })
        ),
        "ZIP64 local extra field was accepted"
    );
}

fn headers(bytes: &[u8], name: &str) -> (usize, usize) {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let entry = zip.by_name(name).unwrap();
    (
        entry.header_start() as usize,
        entry.central_header_start() as usize,
    )
}

#[test]
fn bld62_to_bld68_unsafe_or_unsupported_sources_are_rejected_before_control() {
    let case = Case::body("<p>Hello.</p>");
    for variant in 0..8 {
        let mut files = book_entries("<p>Hello.</p>");
        files.push(entry("last.bin", [1, 2, 3]));
        if variant == 0 {
            for i in 0..4096 {
                files.push(entry(&format!("extra/{i}"), []));
            }
        }
        if variant == 1 {
            files.last_mut().unwrap().name = "../escape".into();
        }
        if variant == 2 {
            files.push(entry("dupe.bin", [1]));
        }
        let mut bytes = zip_bytes(files);
        let diagnostic = match variant {
            0 => "4096",
            1 => "unsafe",
            2 => {
                patch_name(&mut bytes, "dupe.bin", "last.bin");
                "duplicate"
            }
            3 => {
                declared_size(&mut bytes, "last.bin", 32 * 1024 * 1024 + 1);
                "32 MiB"
            }
            4 => {
                let (local, central) = headers(&bytes, "last.bin");
                bytes[local + 8..local + 10].copy_from_slice(&99u16.to_le_bytes());
                bytes[central + 10..central + 12].copy_from_slice(&99u16.to_le_bytes());
                "compression"
            }
            5 => {
                let (local, central) = headers(&bytes, "last.bin");
                bytes[local + 6] |= 1;
                bytes[central + 8] |= 1;
                "encrypted"
            }
            6 => {
                let end = bytes.len() - 22;
                bytes[end + 4..end + 6].copy_from_slice(&1u16.to_le_bytes());
                "multi-disk"
            }
            _ => {
                let end = bytes.len() - 22;
                bytes[end + 10..end + 12].copy_from_slice(&u16::MAX.to_le_bytes());
                bytes[end + 8..end + 10].copy_from_slice(&u16::MAX.to_le_bytes());
                "ZIP64"
            }
        };
        fs::write(&case.book.path, &bytes).unwrap();
        let result = case.build(&[case.source()]);
        failure(&result);
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(diagnostic),
            "variant {variant}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!case.output().exists());
        assert_eq!(fs::read_dir(case.root.path()).unwrap().count(), 1);
    }
}

#[test]
fn bld68_zip_symlinks_and_epub_encryption_are_rejected() {
    let case = Case::body("<p>Hello.</p>");
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for file in book_entries("<p>Hello.</p>") {
        writer
            .start_file(
                file.name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(&file.data).unwrap();
    }
    writer
        .add_symlink(
            "link.bin",
            "../../outside",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    fs::write(&case.book.path, writer.finish().unwrap().into_inner()).unwrap();
    let result = case.build(&[case.source()]);
    failure(&result);
    assert!(String::from_utf8_lossy(&result.stderr).contains("symbolic link"));
    let mut files = book_entries("<p>Hello.</p>");
    files.push(entry("META-INF/encryption.xml", "<encryption/>"));
    fs::write(&case.book.path, zip_bytes(files)).unwrap();
    let result = case.build(&[case.source()]);
    failure(&result);
    assert!(String::from_utf8_lossy(&result.stderr).contains("EPUB encryption"));
    assert!(!case.output().exists());
}

#[test]
fn zip_metadata_is_preserved_while_extra_fields_are_deliberately_omitted() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .set_raw_comment(b"opaque archive comment\xff".to_vec().into_boxed_slice())
        .unwrap();
    for (index, file) in asset_entries().into_iter().enumerate() {
        let mut options = zip::write::FullFileOptions::default()
            .compression_method(file.compression)
            .last_modified_time(zip::DateTime::from_date_and_time(2001, 2, 3, 4, 5, 6).unwrap())
            .system(zip::System::Unix)
            .external_attributes(
                (if file.name.ends_with('/') {
                    0o40750
                } else {
                    0o100640
                }) << 16,
            )
            .with_file_comment("comentário 日本語");
        if index != 0 {
            options.add_extra_field(0xfefe, [1, 2, 3], false).unwrap();
        }
        writer.start_file(file.name, options).unwrap();
        writer.write_all(&file.data).unwrap();
    }
    let case = Case::from_book(Fixture::bytes(&writer.finish().unwrap().into_inner()));
    success(&case.build(&[case.source()]));
    let mut original = ZipArchive::new(File::open(&case.book.path).unwrap()).unwrap();
    let mut rebuilt = ZipArchive::new(File::open(case.output()).unwrap()).unwrap();
    assert_eq!(original.comment(), rebuilt.comment());
    for index in 0..original.len() {
        let before = original.by_index(index).unwrap();
        let after = rebuilt.by_index(index).unwrap();
        assert_eq!(before.name().unwrap(), after.name().unwrap());
        assert_eq!(before.last_modified(), after.last_modified());
        assert_eq!(before.comment(), after.comment());
        assert_eq!(before.system(), after.system());
        assert_eq!(before.external_attributes(), after.external_attributes());
        assert!(after.extra_data_fields().next().is_none());
    }
}

#[test]
fn mimetype_is_written_first_even_if_central_directory_order_differs() {
    let mut bytes = zip_bytes(book_entries("<p>Hello.</p>"));
    let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let start = zip.central_directory_start() as usize;
    let next = zip.by_index(1).unwrap().central_header_start() as usize;
    drop(zip);
    let first = bytes[start..next].to_vec();
    bytes.drain(start..next);
    let end = bytes.len() - 22;
    bytes.splice(end..end, first);
    let case = Case::from_book(Fixture::bytes(&bytes));
    success(&case.build(&[case.source()]));
    let mut output = ZipArchive::new(File::open(case.output()).unwrap()).unwrap();
    assert_eq!(output.by_index(0).unwrap().name().unwrap(), "mimetype");
    assert_eq!(output.by_index(0).unwrap().header_start(), 0);
    assert_eq!(resources(&case.output()), resources(&case.book.path));
}

#[test]
fn no_segments_is_a_complete_translation_with_an_empty_envelope() {
    let case = Case::body("");
    success(&case.build(&[case.source()]));
    assert_eq!(resources(&case.output()), resources(&case.book.path));
    let output = case.root.path().join("without-response.epub");
    assert!(minami::build_epub(&case.book.path, &case.control(), &[], &output).is_err());
    assert!(!output.exists());
}

#[test]
fn legacy_comment_that_cannot_fit_in_utf8_does_not_block_resource_preservation() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for file in book_entries("<p>Hello.</p>") {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .into_full_options()
            .with_file_comment(if file.name == BODY {
                "x".repeat(u16::MAX as usize)
            } else {
                String::new()
            });
        writer.start_file(file.name, options).unwrap();
        writer.write_all(&file.data).unwrap();
    }
    let mut bytes = writer.finish().unwrap().into_inner();
    let (_, central) = headers(&bytes, BODY);
    let name_len =
        u16::from_le_bytes(bytes[central + 28..central + 30].try_into().unwrap()) as usize;
    let extra_len =
        u16::from_le_bytes(bytes[central + 30..central + 32].try_into().unwrap()) as usize;
    let start = central + 46 + name_len + extra_len;
    bytes[start..start + u16::MAX as usize].fill(0x82);
    let case = Case::from_book(Fixture::bytes(&bytes));
    success(&case.build(&[case.source()]));
    assert_eq!(resources(&case.output()), resources(&case.book.path));
    let mut rebuilt = ZipArchive::new(File::open(case.output()).unwrap()).unwrap();
    assert_eq!(rebuilt.by_name(BODY).unwrap().comment(), "");
}
