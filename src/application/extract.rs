use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    MinamiError, NavigationEntry,
    epub::{
        archive, navigation, package, segments,
        xhtml::{self, ByteSpan},
    },
    x::{self, Control, Payload},
};

const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 200_000;

pub(crate) struct Extraction {
    pub control: Control,
    pub source: Payload,
}

/// Extract X 1.0 files into a new directory; existing destinations are never replaced.
pub fn extract_epub(path: &Path, output: &Path) -> Result<(), MinamiError> {
    let destination = output_destination(output)?;
    let data = extract_data(path)?;
    let control = encode(&data.control)?;
    let source = encode(&data.source)?;
    let parent = destination.parent().ok_or_else(|| {
        crate::epub::invalid(
            &output.display().to_string(),
            "output needs a parent directory",
        )
    })?;
    let stage = tempfile::Builder::new()
        .prefix(".minami-extract-")
        .tempdir_in(parent)
        .map_err(|e| {
            io_error(
                "creating staging directory",
                &output.display().to_string(),
                e,
            )
        })?;
    let stage = write_staged(stage, &control, &source)?;
    publish_ready(stage, &destination)
}

fn output_destination(output: &Path) -> Result<PathBuf, MinamiError> {
    let resource = output.display().to_string();
    let name = output
        .file_name()
        .ok_or_else(|| crate::epub::invalid(&resource, "output must name a new directory"))?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|e| io_error("opening output parent", &resource, e))?;
    if !parent.is_dir() {
        return Err(io_error(
            "opening output parent",
            &resource,
            io::Error::new(
                io::ErrorKind::NotADirectory,
                "output parent is not a directory",
            ),
        ));
    }
    let destination = parent.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io_error(
                "creating extraction output",
                &resource,
                io::Error::new(io::ErrorKind::AlreadyExists, "output already exists"),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error("checking extraction output", &resource, e)),
    }
    Ok(destination)
}

fn write_staged(
    stage: tempfile::TempDir,
    control: &[u8],
    source: &[u8],
) -> Result<tempfile::TempDir, MinamiError> {
    for (name, bytes) in [("control.json", control), ("source.json", source)] {
        let path = stage.path().join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| io_error("creating staged X artifact", &path.display().to_string(), e))?;
        file.write_all(bytes)
            .map_err(|e| io_error("writing staged X artifact", &path.display().to_string(), e))?;
    }
    Ok(stage)
}

fn publish_ready(stage: tempfile::TempDir, destination: &Path) -> Result<(), MinamiError> {
    rename_new(stage.path(), destination).map_err(|e| {
        io_error(
            "publishing extraction directory",
            &destination.display().to_string(),
            e,
        )
    })?;
    // Ownership moved to the final name; never clean up the now-vacant temporary name.
    let _ = stage.keep();
    Ok(())
}

#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in publication path",
        ));
    }
    // SAFETY: both pointers reference live NUL-terminated UTF-16 buffers. Zero flags forbid replacement and cross-volume copying.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
fn rename_new(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace publication is supported on Windows and Linux/Android",
    ))
}

pub(crate) fn extract_data(path: &Path) -> Result<Extraction, MinamiError> {
    let (file, digest) = snapshot(path)?;
    from_snapshot(file, digest, &path.display().to_string())
}

fn io_error(operation: &'static str, resource: &str, source: io::Error) -> MinamiError {
    MinamiError::Io {
        operation,
        resource: resource.into(),
        source,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn snapshot(path: &Path) -> Result<(File, String), MinamiError> {
    let resource = path.display().to_string();
    let mut input =
        File::open(path).map_err(|e| io_error("opening EPUB snapshot source", &resource, e))?;
    let before = input
        .metadata()
        .map_err(|e| io_error("reading source metadata", &resource, e))?;
    if before.len() > archive::MAX_ARCHIVE_BYTES {
        return Err(MinamiError::LimitExceeded {
            resource,
            limit: "128 MiB archive",
        });
    }
    let mut output = tempfile::tempfile()
        .map_err(|e| io_error("creating private EPUB snapshot", &resource, e))?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|e| io_error("reading EPUB snapshot", &resource, e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > archive::MAX_ARCHIVE_BYTES {
            return Err(MinamiError::LimitExceeded {
                resource,
                limit: "128 MiB archive snapshot",
            });
        }
        output
            .write_all(&buffer[..n])
            .map_err(|e| io_error("writing private EPUB snapshot", &resource, e))?;
        hash.update(&buffer[..n]);
    }
    let after = input
        .metadata()
        .map_err(|e| io_error("rechecking source metadata", &resource, e))?;
    if before.len() != after.len()
        || total != before.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(crate::epub::invalid(
            &resource,
            "EPUB changed while copying its snapshot",
        ));
    }
    Ok((output, hex(&hash.finalize())))
}

fn sections(
    doc: &xhtml::Document,
    plan: &segments::Segments,
    navigation: &[NavigationEntry],
    path: &str,
    document_id: &str,
    first_id: usize,
) -> Result<Vec<x::Section>, MinamiError> {
    let body = doc.nodes[doc.body]
        .element()
        .ok_or_else(|| crate::epub::invalid(path, "missing body"))?;
    let begin = body.open.1;
    let end = body.close.map_or(begin, |s| s.0);
    let mut boundaries: BTreeMap<usize, Vec<usize>> = BTreeMap::from([(begin, Vec::new())]);
    for entry in navigation.iter().filter(|e| e.path == path) {
        let boundary = match entry.fragment.as_deref() {
            None => begin,
            Some("") => return Err(crate::epub::invalid(path, "empty NCX fragment")),
            Some(fragment) => {
                let node = doc.ids.get(fragment).ok_or_else(|| {
                    crate::epub::invalid(path, format!("NCX fragment #{fragment} does not exist"))
                })?;
                *plan.promotions.get(node).ok_or_else(||crate::epub::invalid(path,format!("NCX fragment #{fragment} splits linguistic flow or targets content outside the supported body boundaries")))?
            }
        };
        if boundary < begin || boundary > end {
            return Err(crate::epub::invalid(path, "NCX boundary outside body"));
        }
        boundaries.entry(boundary).or_default().push(entry.position);
    }
    let boundaries: Vec<_> = boundaries.into_iter().collect();
    Ok(boundaries
        .iter()
        .enumerate()
        .map(|(i, (start, positions))| x::Section {
            id: format!("q{}", first_id + i),
            document_id: document_id.into(),
            range: ByteSpan(*start, boundaries.get(i + 1).map_or(end, |next| next.0)),
            navigation_positions: positions.clone(),
        })
        .collect())
}

fn from_snapshot(file: File, digest: String, resource: &str) -> Result<Extraction, MinamiError> {
    let extraction_id = hex(&Sha256::digest(format!(
        "minami-x\n{}\n{}\n{digest}\n",
        x::VERSION,
        x::PROFILE
    )));
    let mut archive = archive::from_file(file, resource)?;
    let package_path = package::locate(&mut archive)?;
    let package = package::parse(&mut archive, &package_path)?;
    let navigation = navigation::parse(&mut archive, &package.ncx_path, &package.resources)?;
    let mut result = Extraction {
        control: Control {
            format_version: x::VERSION,
            profile: x::PROFILE,
            extraction_id: extraction_id.clone(),
            source: x::Origin {
                epub_sha256: digest,
                epub_version: package.version,
                package_path,
                ncx_path: package.ncx_path,
            },
            spine: package.spine,
            documents: Vec::new(),
            navigation,
            sections: Vec::new(),
            segments: Vec::new(),
            preserved: Vec::new(),
        },
        source: Payload {
            format_version: x::VERSION,
            extraction_id,
            segments: Vec::new(),
        },
    };
    let by_id: BTreeMap<_, _> = package
        .resources
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for entry in &result.control.spine {
        let index = *by_id
            .get(entry.idref.as_str())
            .ok_or_else(|| crate::epub::invalid(resource, "missing spine resource"))?;
        if seen.insert(index) {
            order.push(index);
        }
    }
    for (index, item) in package.resources.iter().enumerate() {
        if item.media_type == crate::epub::XHTML && seen.insert(index) {
            order.push(index);
        }
    }
    let mut records = 0usize;
    let mut text_bytes = 0usize;
    for index in order {
        let resource = &package.resources[index];
        let source = archive.read_xhtml(&resource.path)?;
        let doc = xhtml::parse(&source, &resource.path)?;
        let plan = segments::extract(&doc, &resource.path)?;
        let document_id = format!("d{}", result.control.documents.len() + 1);
        let mapped_sections = sections(
            &doc,
            &plan,
            &result.control.navigation,
            &resource.path,
            &document_id,
            result.control.sections.len() + 1,
        )?;
        records += 1
            + mapped_sections.len()
            + plan.preserved.len()
            + plan
                .segments
                .iter()
                .map(|d| 1 + d.parts.len() + d.markers.len())
                .sum::<usize>();
        text_bytes += plan
            .segments
            .iter()
            .flat_map(|d| &d.parts)
            .filter_map(|p| {
                if let x::Part::Text(t) = p {
                    Some(t.len())
                } else {
                    None
                }
            })
            .sum::<usize>();
        if records > MAX_RECORDS || text_bytes > MAX_JSON_BYTES {
            return Err(MinamiError::LimitExceeded {
                resource: resource.path.clone(),
                limit: "200000 X records or 64 MiB editable text",
            });
        }
        let body = doc.nodes[doc.body]
            .element()
            .ok_or_else(|| crate::epub::invalid(&resource.path, "missing body"))?;
        result.control.documents.push(x::Document {
            id: document_id.clone(),
            manifest_id: resource.id.clone(),
            path: resource.path.clone(),
            body: ByteSpan(body.open.1, body.close.map_or(body.open.1, |s| s.0)),
        });
        let mut section_index = 0;
        for draft in plan.segments {
            while section_index + 1 < mapped_sections.len()
                && mapped_sections[section_index + 1].range.0 <= draft.range.0
            {
                section_index += 1;
            }
            let section = &mapped_sections[section_index];
            let id = format!("s{}", result.control.segments.len() + 1);
            if draft.range.0 < section.range.0 || draft.range.1 > section.range.1 {
                return Err(crate::epub::invalid(
                    &resource.path,
                    format!(
                        "segment {id} crosses section {} at byte {}",
                        section.id, section.range.1
                    ),
                ));
            }
            result.control.segments.push(x::Segment {
                id: id.clone(),
                document_id: document_id.clone(),
                section_id: section.id.clone(),
                range: draft.range,
                markers: draft.markers,
            });
            result.source.segments.push(x::SourceSegment {
                id,
                parts: draft.parts,
            });
        }
        result.control.sections.extend(mapped_sections);
        result
            .control
            .preserved
            .extend(plan.preserved.into_iter().map(|p| x::Preserved {
                document_id: document_id.clone(),
                range: p.range,
                reason: p.reason,
            }));
    }
    Ok(result)
}

struct JsonBuffer {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl Write for JsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("X JSON byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, MinamiError> {
    encode_budget(value, MAX_JSON_BYTES)
}

fn encode_budget(value: &impl serde::Serialize, max: usize) -> Result<Vec<u8>, MinamiError> {
    let mut buffer = JsonBuffer {
        bytes: Vec::new(),
        max,
        exceeded: false,
    };
    let result = serde_json::to_writer_pretty(&mut buffer, value)
        .map_err(io::Error::other)
        .and_then(|()| buffer.write_all(b"\n"));
    if let Err(error) = result {
        if buffer.exceeded {
            return Err(MinamiError::LimitExceeded {
                resource: "X JSON".into(),
                limit: "64 MiB per serialized X artifact",
            });
        }
        return Err(io_error("serializing X", "X JSON", error));
    }
    Ok(buffer.bytes)
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../tests/support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use super::support::*;
    use super::*;
    use crate::{
        epub::xhtml::ByteSpan,
        x::{Part, Token},
    };
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, fs};

    fn wrap(body: &str) -> String {
        format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")
    }

    fn nav(targets: &[&str]) -> String {
        let points=targets.iter().enumerate().map(|(i,t)|format!("<navPoint id='n{i}'><navLabel><text>Section {i}</text></navLabel><content src='{t}'/></navPoint>")).collect::<String>();
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

    fn hash(bytes: impl AsRef<[u8]>) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn d01_same_epub_produces_identical_json_bytes() {
        let f = book("<p>Hello <i>world</i>.</p>");
        let a = extract_data(&f.path).unwrap();
        let b = extract_data(&f.path).unwrap();
        assert_eq!(encode(&a.control).unwrap(), encode(&b.control).unwrap());
        assert_eq!(encode(&a.source).unwrap(), encode(&b.source).unwrap());
        let source = String::from_utf8(encode(&a.source).unwrap()).unwrap();
        assert!(source.ends_with('\n') && source.contains("\n  \"format_version\""));
        assert!(!source.contains("OEBPS") && !source.contains("range") && !source.contains("href"));
    }

    #[test]
    fn d02_complete_zip_digest_and_contract_identity() {
        let f = book("<p>Digest</p>");
        let digest = hash(fs::read(&f.path).unwrap());
        let data = extract_data(&f.path).unwrap();
        assert_eq!(data.control.source.epub_sha256, digest);
        let expected = hash(format!("minami-x\n1.0\nepub2-xhtml-v1\n{digest}\n"));
        assert_eq!(data.control.extraction_id, expected);
        assert_eq!(data.source.extraction_id, expected);
        assert_eq!(data.control.profile, "epub2-xhtml-v1");
    }

    #[test]
    fn d03_unique_documents_keep_spine_order_and_repeated_references() {
        let mut entries = book_entries("<p>Body</p>", &["Text/Body.xhtml"]);
        replace(
            &mut entries,
            "OEBPS/content.opf",
            OPF.replace("</spine>", "<itemref idref='cover'/></spine>"),
        );
        let f = Fixture::new(entries);
        let out = extract_data(&f.path).unwrap();
        assert_eq!(
            out.control
                .documents
                .iter()
                .map(|d| d.manifest_id.as_str())
                .collect::<Vec<_>>(),
            ["cover", "illustrations", "body"]
        );
        assert_eq!(out.control.spine.len(), 4);
        assert!(!out.control.spine[1].linear);
        assert_eq!(out.control.segments.len(), 1);
    }

    #[test]
    fn d04_manifest_xhtml_outside_spine_is_covered() {
        let mut entries = book_entries("<p>Body</p>", &["Text/Body.xhtml"]);
        replace(
            &mut entries,
            "OEBPS/content.opf",
            OPF.replace("<itemref idref=\"illustrations\" linear=\"no\"/>", ""),
        );
        replace(
            &mut entries,
            "OEBPS/Text/Illustrations.xhtml",
            wrap("<p>Unindexed</p>"),
        );
        let f = Fixture::new(entries);
        let out = extract_data(&f.path).unwrap();
        assert_eq!(out.control.documents[2].manifest_id, "illustrations");
        assert_eq!(
            out.source.segments[1].parts,
            [Part::Text("Unindexed".into())]
        );
        assert!(
            out.control
                .sections
                .last()
                .unwrap()
                .navigation_positions
                .is_empty()
        );
    }

    #[test]
    fn d05_missing_empty_and_non_body_fragments_fail() {
        for target in ["Text/Body.xhtml#missing", "Text/Body.xhtml#"] {
            let f = Fixture::new(book_entries("<p>Text</p>", &[target]));
            assert!(extract_data(&f.path).is_err(), "{target}");
        }
        let mut entries = book_entries("", &["Text/Body.xhtml#head"]);
        replace(
            &mut entries,
            "OEBPS/Text/Body.xhtml",
            "<html xmlns='http://www.w3.org/1999/xhtml'><head><title id='head'>Head</title></head><body><p>Text</p></body></html>",
        );
        assert!(extract_data(&Fixture::new(entries).path).is_err());
    }

    #[test]
    fn d06_multiple_sections_and_aliases_share_one_document() {
        let f = Fixture::new(book_entries(
            "<h1 id='one'>One</h1><p>A</p><h1 id='two'>Two</h1><p>B</p>",
            &[
                "Text/Body.xhtml#one",
                "Text/Body.xhtml#two",
                "Text/Body.xhtml#one",
            ],
        ));
        let out = extract_data(&f.path).unwrap();
        let sections: Vec<_> = out
            .control
            .sections
            .iter()
            .filter(|s| s.document_id == "d3")
            .collect();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].navigation_positions, [0, 2]);
        assert_eq!(sections[1].navigation_positions, [1]);
        assert_eq!(
            out.control.segments[0].section_id,
            out.control.segments[1].section_id
        );
        assert_ne!(
            out.control.segments[1].section_id,
            out.control.segments[2].section_id
        );
    }

    #[test]
    fn d07_prefix_and_document_without_navigation_are_not_dropped() {
        let f = Fixture::new(book_entries(
            "<p>Prelude</p><h1 id='chapter'>Chapter</h1><p>Text</p>",
            &["Text/Body.xhtml#chapter"],
        ));
        let out = extract_data(&f.path).unwrap();
        let prefix = &out.control.sections[2];
        assert!(prefix.navigation_positions.is_empty());
        assert_eq!(out.control.segments[0].section_id, prefix.id);
        assert_eq!(out.source.segments.len(), 3);
    }

    #[test]
    fn d08_mid_phrase_boundary_fails_but_leading_inline_anchor_promotes() {
        let bad = Fixture::new(book_entries(
            "<p>Before <span id='c'>inside</span> after.</p>",
            &["Text/Body.xhtml#c"],
        ));
        assert!(extract_data(&bad.path).is_err());
        let good = Fixture::new(book_entries(
            "<p> <i><a id='c'></a>Text</i></p>",
            &["Text/Body.xhtml#c"],
        ));
        let out = extract_data(&good.path).unwrap();
        assert_eq!(out.source.segments.len(), 1);
        let segment = &out.control.segments[0];
        let section = out
            .control
            .sections
            .iter()
            .find(|q| q.id == segment.section_id)
            .unwrap();
        assert_eq!(segment.range.0, section.range.0);
    }

    #[test]
    fn d09_spans_are_inside_sections_and_segments() {
        let body = "<p>A<i class='x'>B<br/>C</i><a id='e'></a> D</p>";
        let src = wrap(body);
        let f = book(body);
        let out = extract_data(&f.path).unwrap();
        let segment = &out.control.segments[0];
        let q = out
            .control
            .sections
            .iter()
            .find(|q| q.id == segment.section_id)
            .unwrap();
        assert!(q.range.0 <= segment.range.0 && segment.range.1 <= q.range.1);
        for marker in &segment.markers {
            let spans = match marker.token {
                Token::Pair { open, close, .. } => vec![open, close],
                Token::Atom { range, .. } => vec![range],
            };
            for ByteSpan(a, b) in spans {
                assert!(segment.range.0 <= a && a <= b && b <= segment.range.1);
                assert!(src.is_char_boundary(a) && src.is_char_boundary(b));
            }
        }
    }

    fn assert_rejected_ncx_boundary(body: &str, fragment: &str) {
        let valid = book(body);
        assert!(extract_data(&valid.path).is_ok());
        let target = format!("Text/Body.xhtml#{fragment}");
        let fixture = Fixture::new(book_entries(body, &[&target]));
        let before = fs::read(&fixture.path).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let result = extract_epub(&fixture.path, &parent.path().join("work"));
        assert!(
            matches!(&result, Err(MinamiError::InvalidReference { resource, detail })
                if resource == "OEBPS/Text/Body.xhtml"
                    && detail.contains(&format!("NCX fragment #{fragment}"))
                    && detail.contains("supported body boundaries")),
            "expected an NCX boundary error, got {result:?}"
        );
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }

    #[test]
    fn n01_ncx_callout_root_and_descendants_are_opaque() {
        for (body, fragment) in [
            (
                "<p><sup id='ref'><a href='#note'>[1]</a></sup>Text</p>",
                "ref",
            ),
            (
                "<p><sup><a id='ref' href='#note'>[1]</a></sup>Text</p>",
                "ref",
            ),
            (
                "<p><sub id='ref'><a href='#note'>[1]</a></sub>Text</p>",
                "ref",
            ),
        ] {
            assert_rejected_ncx_boundary(body, fragment);
        }
    }

    #[test]
    fn n02_ncx_xhtml_id_inside_svg_cannot_define_a_section() {
        assert_rejected_ncx_boundary(
            "<p><svg xmlns='http://www.w3.org/2000/svg'><foreignObject><a xmlns='http://www.w3.org/1999/xhtml' id='chapter'>Protected</a></foreignObject></svg>Text</p>",
            "chapter",
        );
    }

    #[test]
    fn n03_leading_empty_anchor_keeps_section_and_original_atom() {
        let body = "<p> <i><a id='chapter'></a>Text</i></p>";
        let fixture = Fixture::new(book_entries(body, &["Text/Body.xhtml#chapter"]));
        let data = extract_data(&fixture.path).unwrap();
        assert_eq!(data.control.segments.len(), 1);
        let segment = &data.control.segments[0];
        let section = data
            .control
            .sections
            .iter()
            .find(|q| q.id == segment.section_id)
            .unwrap();
        assert_eq!(section.document_id, segment.document_id);
        assert_eq!(section.navigation_positions, [0]);
        assert_eq!(section.range.0, segment.range.0);
        assert!(segment.range.1 <= section.range.1);
        let Token::Atom { range, parent } = &segment.markers[1].token else {
            panic!("empty anchor must remain one atom");
        };
        assert_eq!(parent.as_deref(), Some("i1"));
        assert_eq!(&wrap(body)[range.0..range.1], "<a id='chapter'></a>");
        assert_eq!(
            data.source.segments[0].parts,
            [
                Part::Text(" ".into()),
                Part::Open("i1".into()),
                Part::Atom("a2".into()),
                Part::Text("Text".into()),
                Part::Close("i1".into()),
            ]
        );
        let again = extract_data(&fixture.path).unwrap();
        assert_eq!(
            encode(&data.control).unwrap(),
            encode(&again.control).unwrap()
        );
        assert_eq!(
            encode(&data.source).unwrap(),
            encode(&again.source).unwrap()
        );
    }

    #[test]
    fn n04_empty_anchor_after_linguistic_text_cannot_split_a_group() {
        assert_rejected_ncx_boundary("<p>Before <a id='chapter'></a> after.</p>", "chapter");
    }

    #[test]
    fn d10_original_tokens_can_be_copied_without_xml_serialization() {
        let body = "<p>A<i class='x'>B<br/>C</i><a id='e'></a> D</p>";
        let src = wrap(body);
        let f = book(body);
        let out = extract_data(&f.path).unwrap();
        let segment = &out.control.segments[0];
        let markers: BTreeMap<_, _> = segment
            .markers
            .iter()
            .map(|m| (m.id.as_str(), &m.token))
            .collect();
        let mut copied = String::new();
        for part in &out.source.segments[0].parts {
            match part {
                Part::Text(t) => copied.push_str(t),
                Part::Open(id) => {
                    if let Token::Pair { open, .. } = markers[id.as_str()] {
                        copied.push_str(&src[open.0..open.1]);
                    }
                }
                Part::Close(id) => {
                    if let Token::Pair { close, .. } = markers[id.as_str()] {
                        copied.push_str(&src[close.0..close.1]);
                    }
                }
                Part::Atom(id) => {
                    if let Token::Atom { range, .. } = markers[id.as_str()] {
                        copied.push_str(&src[range.0..range.1]);
                    }
                }
            }
        }
        assert_eq!(copied, &src[segment.range.0..segment.range.1]);
    }

    #[test]
    fn snapshot_mapping_uses_the_hashed_bytes_even_if_original_changes() {
        let f = book("<p>Frozen</p>");
        let original = fs::read(&f.path).unwrap();
        let (file, digest) = snapshot(&f.path).unwrap();
        fs::write(&f.path, b"changed").unwrap();
        let out = from_snapshot(file, digest, "original.epub").unwrap();
        assert_eq!(out.control.source.epub_sha256, hash(original));
        assert_eq!(out.source.segments[0].parts, [Part::Text("Frozen".into())]);
    }

    #[test]
    fn output_and_snapshot_budgets_fail_explicitly() {
        assert!(matches!(
            encode_budget(&"too long", 4),
            Err(MinamiError::LimitExceeded { .. })
        ));
        let f = book("<p>Text</p>");
        File::options()
            .write(true)
            .open(&f.path)
            .unwrap()
            .set_len(128 * 1024 * 1024 + 1)
            .unwrap();
        assert!(matches!(
            snapshot(&f.path),
            Err(MinamiError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn invalid_utf8_is_not_a_partial_extraction() {
        let mut e = book_entries("<p>Text</p>", &["Text/Body.xhtml"]);
        replace(&mut e, "OEBPS/Text/Body.xhtml", [0xff]);
        let f = Fixture::new(e);
        assert!(matches!(
            extract_data(&f.path),
            Err(MinamiError::MalformedXml { .. })
        ));
    }

    #[test]
    fn normative_control_and_source_example_match_exactly() {
        let spec = include_str!("../../docs/x-format.md");
        let raw = spec
            .split("```html\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap();
        let mut entries = book_entries("", &["Text/Body.xhtml"]);
        replace(&mut entries, "OEBPS/Text/Body.xhtml", raw);
        replace(
            &mut entries,
            "OEBPS/content.opf",
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0"><manifest><item id="body" href="Text/Body.xhtml" media-type="application/xhtml+xml"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"><itemref idref="body"/></spine></package>"#,
        );
        replace(
            &mut entries,
            "OEBPS/toc.ncx",
            r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap><navPoint id="n1"><navLabel><text>First</text></navLabel><content src="Text/Body.xhtml"/></navPoint><navPoint id="n2"><navLabel><text>Next</text></navLabel><content src="Text/Body.xhtml#next"/></navPoint></navMap></ncx>"#,
        );
        let f = Fixture::new(entries);
        let out = extract_data(&f.path).unwrap();
        let mut examples = spec.split("```json\n").skip(1);
        let mut expected: serde_json::Value =
            serde_json::from_str(examples.next().unwrap().split("\n```").next().unwrap()).unwrap();
        expected["source"]["epub_sha256"] = out.control.source.epub_sha256.clone().into();
        expected["extraction_id"] = out.control.extraction_id.clone().into();
        assert_eq!(serde_json::to_value(&out.control).unwrap(), expected);
        let mut expected: serde_json::Value =
            serde_json::from_str(examples.next().unwrap().split("\n```").next().unwrap()).unwrap();
        expected["extraction_id"] = out.source.extraction_id.clone().into();
        assert_eq!(serde_json::to_value(&out.source).unwrap(), expected);
    }

    #[test]
    fn failed_second_staged_write_removes_only_owned_temporary_data() {
        let parent = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir_in(parent.path()).unwrap();
        let stage_path = stage.path().to_owned();
        fs::create_dir(stage.path().join("source.json")).unwrap();
        let sentinel = parent.path().join("keep");
        fs::write(&sentinel, b"user data").unwrap();
        assert!(matches!(
            write_staged(stage, b"control", b"source"),
            Err(MinamiError::Io { .. })
        ));
        assert!(!stage_path.exists());
        assert_eq!(fs::read(sentinel).unwrap(), b"user data");
    }

    #[test]
    fn destination_created_after_preflight_is_not_replaced_even_if_empty() {
        let parent = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir_in(parent.path()).unwrap();
        fs::write(stage.path().join("control.json"), b"control").unwrap();
        fs::write(stage.path().join("source.json"), b"source").unwrap();
        let stage_path = stage.path().to_owned();
        let output = parent.path().join("output");
        fs::create_dir(&output).unwrap();
        assert!(matches!(
            publish_ready(stage, &output),
            Err(MinamiError::Io { .. })
        ));
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        assert!(!stage_path.exists());
    }

    #[test]
    fn different_inline_targets_can_normalize_to_one_boundary() {
        let f = Fixture::new(book_entries(
            "<p> <span id='outer'><a id='inner'></a>Text</span></p>",
            &["Text/Body.xhtml#inner", "Text/Body.xhtml#outer"],
        ));
        let out = extract_data(&f.path).unwrap();
        let associated: Vec<_> = out
            .control
            .sections
            .iter()
            .filter(|q| !q.navigation_positions.is_empty())
            .collect();
        assert_eq!(associated.len(), 1);
        assert_eq!(associated[0].navigation_positions, [0, 1]);
        assert_eq!(out.control.segments[0].section_id, associated[0].id);
    }
}
