mod support;

use std::{fs, io::Cursor, process::Command};

use minami::{MinamiError, inspect_epub};
use support::*;
use zip::ZipArchive;

#[test]
fn ins01_minimal_epub2() {
    let book = Fixture::new(entries());
    let report = inspect_epub(&book.path).unwrap();
    assert_eq!(report.epub_version, "2.0");
    assert_eq!(report.package_path, "OEBPS/content.opf");
    assert_eq!(report.resources.len(), 4);
    assert_eq!(report.resources[1].path, "OEBPS/Text/Body.xhtml");
    assert_eq!(report.resources[1].media_type, "application/xhtml+xml");
}

#[test]
fn ins02_spine_order_overrides_zip_and_names() {
    let book = Fixture::new(entries());
    let report = inspect_epub(&book.path).unwrap();
    assert_eq!(
        report
            .spine
            .iter()
            .map(|e| e.idref.as_str())
            .collect::<Vec<_>>(),
        ["cover", "illustrations", "body"]
    );
    assert!(report.spine[0].linear);
    assert!(!report.spine[1].linear);
}

#[test]
fn ins03_sections_share_xhtml_and_preserve_hierarchy() {
    let book = Fixture::new(entries());
    let report = inspect_epub(&book.path).unwrap();
    assert_eq!(report.navigation.len(), 3);
    for (i, (entry, fragment)) in report
        .navigation
        .iter()
        .zip(["one", "two", "three"])
        .enumerate()
    {
        assert_eq!(entry.path, "OEBPS/Text/Body.xhtml");
        assert_eq!(entry.fragment.as_deref(), Some(fragment));
        assert_eq!(entry.position, i);
    }
    assert_eq!(
        report
            .navigation
            .iter()
            .map(|n| n.depth)
            .collect::<Vec<_>>(),
        [0, 1, 0]
    );
    assert_eq!(report.navigation[0].id.as_deref(), Some("one"));
    assert_eq!(report.navigation[0].title, "First");
    assert_eq!(report.navigation[2].id, None);
}

#[test]
fn ins04_navigation_without_fragment() {
    let mut files = entries();
    replace(&mut files, "OEBPS/toc.ncx", NCX.replace("#one", ""));
    let book = Fixture::new(files);
    let report = inspect_epub(&book.path).unwrap();
    assert_eq!(report.navigation[0].fragment, None);
    assert_eq!(report.navigation[0].path, "OEBPS/Text/Body.xhtml");
}

#[test]
fn ins05_determinism() {
    let book = Fixture::new(entries());
    let first = inspect_epub(&book.path).unwrap();
    let second = inspect_epub(&book.path).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
}

#[test]
fn ins06_invalid_references() {
    let mut missing_id = entries();
    replace(
        &mut missing_id,
        "OEBPS/content.opf",
        OPF.replace("idref=\"body\"", "idref=\"absent\""),
    );
    let mut missing_file = entries();
    missing_file.retain(|e| e.name != "OEBPS/Text/Body.xhtml");
    let mut bad_target = entries();
    replace(
        &mut bad_target,
        "OEBPS/toc.ncx",
        NCX.replace("Text/Body.xhtml#one", "Text/Absent.xhtml"),
    );
    let mut unlisted_target = entries();
    unlisted_target.push(entry("OEBPS/other.xhtml", "<html/>"));
    replace(
        &mut unlisted_target,
        "OEBPS/toc.ncx",
        NCX.replace("Text/Body.xhtml#one", "other.xhtml"),
    );
    for files in [missing_id, missing_file, bad_target, unlisted_target] {
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::InvalidReference { .. })
        ));
    }
}

#[test]
fn ins07_invalid_container() {
    let corrupt = Fixture::bytes(b"not a zip");
    assert!(matches!(
        inspect_epub(&corrupt.path),
        Err(MinamiError::InvalidZip { .. })
    ));
    for mode in 0..4 {
        let mut files = entries();
        match mode {
            0 => replace(&mut files, "mimetype", "application/epub+zip\n"),
            1 => files[0].compression = zip::CompressionMethod::Deflated,
            2 => files.swap(0, 1),
            _ => {
                files.remove(0);
            }
        }
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::MissingStructure { .. } | MinamiError::InvalidReference { .. })
        ));
    }
    let mut files = entries();
    files.retain(|e| e.name != "META-INF/container.xml");
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::MissingStructure { .. })
    ));
}

#[test]
fn ins08_malformed_xml() {
    for path in [
        "META-INF/container.xml",
        "OEBPS/content.opf",
        "OEBPS/toc.ncx",
    ] {
        let mut files = entries();
        replace(&mut files, path, "<broken>");
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::MalformedXml { .. })
        ));
    }
}

#[test]
fn ins09_unsafe_paths_and_duplicates() {
    for name in [
        "../escape",
        "/absolute",
        "C:/absolute",
        "OEBPS/../escape",
        "OEBPS\\escape",
        "OEBPS//alias",
        "OEBPS/./alias",
    ] {
        let mut files = entries();
        files.push(entry(name, ""));
        let book = Fixture::new(files);
        assert!(
            matches!(
                inspect_epub(&book.path),
                Err(MinamiError::InvalidReference { .. })
            ),
            "{name}"
        );
    }
    let mut files = entries();
    files.push(entry("sameA", ""));
    files.push(entry("sameB", ""));
    let mut bytes = zip_bytes(files);
    patch_name(&mut bytes, "sameB", "sameA");
    let book = Fixture::bytes(&bytes);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::InvalidZip { .. } | MinamiError::AmbiguousStructure { .. })
    ));
    for target in [
        "../../outside.xhtml",
        "%2e%2e/%2e%2e/outside.xhtml",
        "/absolute.xhtml",
        "https://example.com/book.xhtml",
        "Text%2fBody.xhtml#one",
        "Text/Body.xhtml?x=1",
    ] {
        let mut files = entries();
        replace(
            &mut files,
            "OEBPS/toc.ncx",
            NCX.replace("Text/Body.xhtml#one", target),
        );
        let book = Fixture::new(files);
        assert!(
            matches!(
                inspect_epub(&book.path),
                Err(MinamiError::InvalidReference { .. } | MinamiError::Unsupported { .. })
            ),
            "{target}"
        );
    }
}

#[test]
fn ins10_resource_limits() {
    let mut too_many = entries();
    for i in 0..4096 {
        too_many.push(entry(&format!("extra/{i}"), ""));
    }
    let book = Fixture::new(too_many);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut files = entries();
    files.push(Entry {
        name: "large".into(),
        data: vec![b'x'; 32 * 1024 * 1024 + 1],
        compression: zip::CompressionMethod::Deflated,
    });
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut files = entries();
    for i in 0..9 {
        files.push(entry(&format!("large{i}"), ""));
    }
    let mut bytes = zip_bytes(files);
    for i in 0..9 {
        declared_size(&mut bytes, &format!("large{i}"), 32 * 1024 * 1024);
    }
    let book = Fixture::bytes(&bytes);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut files = entries();
    replace(&mut files, "OEBPS/toc.ncx", vec![b' '; 4 * 1024 * 1024 + 1]);
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));
}

#[test]
fn ins11_input_bytes_are_immutable() {
    let book = Fixture::new(entries());
    let before = fs::read(&book.path).unwrap();
    inspect_epub(&book.path).unwrap();
    assert_eq!(before, fs::read(&book.path).unwrap());
    let invalid = Fixture::bytes(b"invalid epub");
    let before = fs::read(&invalid.path).unwrap();
    assert!(inspect_epub(&invalid.path).is_err());
    assert_eq!(before, fs::read(&invalid.path).unwrap());
}

#[test]
fn ambiguous_and_missing_structures() {
    let mut ambiguous = entries();
    replace(&mut ambiguous, "META-INF/container.xml", CONTAINER.replace("</rootfiles>", "<rootfile full-path=\"OEBPS/second.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles>"));
    let mut duplicate_id = entries();
    replace(
        &mut duplicate_id,
        "OEBPS/content.opf",
        OPF.replace("id=\"body\"", "id=\"cover\""),
    );
    for files in [ambiguous, duplicate_id] {
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::AmbiguousStructure { .. })
        ));
    }
    for (path, data) in [
        (
            "META-INF/container.xml",
            CONTAINER.replace("<rootfile ", "<wrong "),
        ),
        (
            "OEBPS/content.opf",
            OPF.replace("<spine ", "<wrong ")
                .replace("</spine>", "</wrong>"),
        ),
        (
            "OEBPS/toc.ncx",
            NCX.replace("<navMap>", "<wrong>")
                .replace("</navMap>", "</wrong>"),
        ),
    ] {
        let mut files = entries();
        replace(&mut files, path, data);
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::MissingStructure { .. })
        ));
    }
}

#[test]
fn unsupported_reading_formats_are_explicit() {
    for opf in [
        OPF.replace("version=\"2.0\"", "version=\"3.0\""),
        OPF.replace("application/xhtml+xml", "image/svg+xml"),
    ] {
        let mut files = entries();
        replace(&mut files, "OEBPS/content.opf", opf);
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::Unsupported { .. })
        ));
    }
}

#[test]
fn required_payload_crc_is_checked() {
    let mut bytes = zip_bytes(entries());
    let marker = b"<body/>";
    let i = bytes
        .windows(marker.len())
        .position(|w| w == marker)
        .unwrap();
    bytes[i + 1] = b'X';
    let book = Fixture::bytes(&bytes);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::InvalidZip { .. })
    ));
}

#[test]
fn cli_success_and_failure_are_atomic() {
    let valid = Fixture::new(entries());
    let output = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(&valid.path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["spine"].as_array().unwrap().len(), 3);
    let mut files = entries();
    replace(&mut files, "OEBPS/toc.ncx", "<broken>");
    let invalid = Fixture::new(files);
    let output = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(&invalid.path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OEBPS/toc.ncx"));
}

#[test]
fn relative_paths_and_percent_encoding_resolve_from_each_document() {
    let mut files = entries();
    replace(
        &mut files,
        "OEBPS/content.opf",
        OPF.replace("toc.ncx", "Navigation/toc.ncx")
            .replace("Text/Body.xhtml", "./Text/Body%20name.xhtml"),
    );
    replace(
        &mut files,
        "OEBPS/toc.ncx",
        NCX.replace("Text/Body.xhtml", "../Text/Body%20name.xhtml")
            .replace("#one", "#se%C3%A7%C3%A3o"),
    );
    for e in &mut files {
        if e.name == "OEBPS/toc.ncx" {
            e.name = "OEBPS/Navigation/toc.ncx".into();
        }
        if e.name == "OEBPS/Text/Body.xhtml" {
            e.name = "OEBPS/Text/Body name.xhtml".into();
        }
    }
    let book = Fixture::new(files);
    let report = inspect_epub(&book.path).unwrap();
    assert_eq!(report.navigation[0].path, "OEBPS/Text/Body name.xhtml");
    assert_eq!(report.navigation[0].fragment.as_deref(), Some("seção"));
    assert_eq!(report.resources[1].path, report.navigation[0].path);
}

#[test]
fn xml_entities_never_read_external_resources() {
    for uri in [
        "file:///nonexistent/minami-secret",
        "http://127.0.0.1:9/secret",
    ] {
        let ncx = NCX.replace(
            "<!DOCTYPE ncx PUBLIC \"-//NISO//DTD ncx 2005-1//EN\" \"http://www.daisy.org/z3986/2005/ncx-2005-1.dtd\">",
            &format!("<!DOCTYPE ncx [<!ENTITY secret SYSTEM \"{uri}\">]>")
        ).replace("First", "&secret;");
        let mut files = entries();
        replace(&mut files, "OEBPS/toc.ncx", ncx);
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::Unsupported { .. })
        ));
    }
    let mut files = entries();
    let ncx = NCX.replace("<!DOCTYPE ncx PUBLIC \"-//NISO//DTD ncx 2005-1//EN\" \"http://www.daisy.org/z3986/2005/ncx-2005-1.dtd\">", "<!DOCTYPE ncx [<!ENTITY amplified 'text'>]>").replace("First", "&amplified;");
    replace(&mut files, "OEBPS/toc.ncx", ncx);
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::Unsupported { .. })
    ));
}

#[test]
fn xml_and_navigation_budgets_are_enforced() {
    let mut files = entries();
    let many_nodes = format!(
        "<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">{}</ncx>",
        "<extra/>".repeat(100_001)
    );
    replace(&mut files, "OEBPS/toc.ncx", many_nodes);
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut files = entries();
    let nav_point =
        "<navPoint><navLabel><text>Nested</text></navLabel><content src=\"Text/Body.xhtml\"/>";
    let deeply_nested = format!(
        "<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\"><navMap>{}{}</navMap></ncx>",
        nav_point.repeat(66),
        "</navPoint>".repeat(66)
    );
    replace(&mut files, "OEBPS/toc.ncx", deeply_nested);
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut files = entries();
    files.push(entry(&"x".repeat(1025), ""));
    let book = Fixture::new(files);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));

    let mut bytes = zip_bytes(entries());
    let end = bytes.len() - 22;
    bytes[end + 12..end + 16].copy_from_slice(&(8u32 * 1024 * 1024 + 1).to_le_bytes());
    let book = Fixture::bytes(&bytes);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::LimitExceeded { .. })
    ));
}

#[test]
fn unsupported_encryption_and_xml_base_are_explicit() {
    let mut encrypted = entries();
    encrypted.push(entry("META-INF/encryption.xml", "<encryption/>"));
    let mut xml_base = entries();
    replace(
        &mut xml_base,
        "OEBPS/content.opf",
        OPF.replace("<manifest>", "<manifest xml:base=\"https://example.com/\">"),
    );
    for files in [encrypted, xml_base] {
        let book = Fixture::new(files);
        assert!(matches!(
            inspect_epub(&book.path),
            Err(MinamiError::Unsupported { .. })
        ));
    }
}

#[test]
fn invalid_required_structure_is_never_a_partial_success() {
    let cases = [
        (
            "META-INF/container.xml",
            CONTAINER.replace("OEBPS/content.opf", "absent.opf"),
        ),
        (
            "META-INF/container.xml",
            CONTAINER.replace("OEBPS/content.opf", "../outside.opf"),
        ),
        (
            "OEBPS/content.opf",
            OPF.replace("toc=\"ncx\"", "toc=\"absent\""),
        ),
        (
            "OEBPS/content.opf",
            OPF.replace("<manifest>", "<manifest/><manifest>"),
        ),
        (
            "OEBPS/content.opf",
            OPF.replace("linear=\"no\"", "linear=\"maybe\""),
        ),
        (
            "OEBPS/toc.ncx",
            NCX.replace("<navMap>", "<navMap/><navMap>"),
        ),
        (
            "OEBPS/toc.ncx",
            NCX.replace("<content src=\"Text/Body.xhtml#one\"/>", ""),
        ),
        (
            "OEBPS/toc.ncx",
            NCX.replace("<navMap>", "<navMap><content src=\"Text/Body.xhtml\"/>"),
        ),
        ("OEBPS/toc.ncx", NCX.replace("id=\"two\"", "id=\"one\"")),
        (
            "OEBPS/toc.ncx",
            NCX.replace("playOrder=\"2\"", "playOrder=\"zero\""),
        ),
    ];
    for (path, data) in cases {
        let mut files = entries();
        replace(&mut files, path, &data);
        let book = Fixture::new(files);
        assert!(
            inspect_epub(&book.path).is_err(),
            "accepted invalid {path}: {data}"
        );
    }
}

#[test]
fn inconsistent_zip_headers_and_lying_sizes_are_rejected() {
    let original = zip_bytes(entries());
    let mut wrong_name = original.clone();
    let name = b"OEBPS/Text/Body.xhtml";
    let local_name = wrong_name
        .windows(name.len())
        .position(|w| w == name)
        .unwrap();
    wrong_name[local_name] = b'X';
    let book = Fixture::bytes(&wrong_name);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::InvalidZip { .. })
    ));

    let mut wrong_size = original;
    declared_size(&mut wrong_size, "OEBPS/Text/Body.xhtml", 1);
    let local_header = local_name - 30;
    wrong_size[local_header + 22..local_header + 26].copy_from_slice(&1u32.to_le_bytes());
    let book = Fixture::bytes(&wrong_size);
    assert!(matches!(
        inspect_epub(&book.path),
        Err(MinamiError::InvalidZip { .. })
    ));
}

#[test]
fn zip_comment_can_contain_an_end_record_signature() {
    let mut bytes = zip_bytes(entries());
    let comment = b"opaque comment PK\x05\x06 trailing bytes";
    let end = bytes.len() - 22;
    bytes[end + 20..end + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    bytes.extend_from_slice(comment);
    let book = Fixture::bytes(&bytes);
    assert_eq!(inspect_epub(&book.path).unwrap().spine.len(), 3);
}

#[test]
fn mimetype_local_extra_field_is_rejected() {
    let valid = Fixture::new(entries());
    assert_eq!(inspect_epub(&valid.path).unwrap().epub_version, "2.0");

    let bytes = zip_bytes_with_mimetype_local_extra(entries());
    let mut zip = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    zip.validate_header_consistency().unwrap();
    let mimetype = zip.by_name("mimetype").unwrap();
    assert_eq!(mimetype.extra_data(), None);
    assert_eq!(mimetype.data_start(), Some(30 + 8 + 9));
    drop(mimetype);
    drop(zip);

    let end = bytes
        .windows(22)
        .enumerate()
        .rfind(|(i, w)| {
            &w[..4] == b"PK\x05\x06"
                && i + 22 + usize::from(u16::from_le_bytes([w[20], w[21]])) == bytes.len()
        })
        .map(|(i, _)| i)
        .unwrap();
    let central = u32::from_le_bytes([
        bytes[end + 16],
        bytes[end + 17],
        bytes[end + 18],
        bytes[end + 19],
    ]) as usize;
    assert_eq!(
        u16::from_le_bytes([bytes[central + 30], bytes[central + 31]]),
        0
    );

    let invalid = Fixture::bytes(&bytes);
    assert!(
        matches!(
            inspect_epub(&invalid.path),
            Err(MinamiError::InvalidZip { resource, .. }) if resource == "mimetype"
        ),
        "accepted a structurally valid ZIP with a mimetype local extra field"
    );
}

#[test]
fn io_and_cli_argument_errors_are_explicit() {
    let book = Fixture::new(entries());
    let missing_path = book.path.clone();
    drop(book);
    assert!(matches!(
        inspect_epub(&missing_path),
        Err(MinamiError::Io { .. })
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_minami"))
        .arg("inspect")
        .arg(&missing_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    for args in [vec!["inspect"], vec!["extract", "anything.epub"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_minami"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
