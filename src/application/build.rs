use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use zip::ZipArchive;

use super::{
    publish::{io_error, output_destination, rename_new},
    reconstruct::reconstruct_prepared,
    validate::{PreparedTranslation, prepare_reconstruction},
};
use crate::{MinamiError, epub::packaging};

/// Build and verify a translated EPUB, publishing only to a new destination.
pub fn build_epub(
    original: &Path,
    control: &Path,
    translations: &[PathBuf],
    output: &Path,
) -> Result<(), MinamiError> {
    let destination = output_destination(output)?;
    let prepared = prepare_reconstruction(original, control, translations)?;
    build_prepared(prepared, &destination)
}

fn build_prepared(prepared: PreparedTranslation, destination: &Path) -> Result<(), MinamiError> {
    build_prepared_with(prepared, destination, packaging::write)
}

fn build_prepared_with(
    prepared: PreparedTranslation,
    destination: &Path,
    write: impl FnOnce(
        &mut ZipArchive<File>,
        &BTreeMap<String, String>,
        &mut File,
    ) -> Result<(), MinamiError>,
) -> Result<(), MinamiError> {
    let documents = reconstruct_prepared(&prepared)?;
    if documents.len() != prepared.extraction().control.documents.len()
        || documents
            .iter()
            .zip(&prepared.extraction().control.documents)
            .any(|(actual, expected)| {
                actual.document_id != expected.id || actual.path != expected.path
            })
    {
        return Err(crate::epub::invalid(
            "rebuilt XHTML",
            "document identities differ from the approved extraction",
        ));
    }
    let xhtml: BTreeMap<_, _> = documents
        .into_iter()
        .map(|document| (document.path, document.source))
        .collect();
    // Moving the archive also releases the extraction and validated response buffers.
    let mut original = prepared.into_archive().into_zip();
    let stage = tempfile::Builder::new()
        .prefix(".minami-build-")
        .tempdir_in(destination.parent().unwrap_or(Path::new(".")))
        .map_err(|error| {
            io_error(
                "creating EPUB staging directory",
                &destination.display().to_string(),
                error,
            )
        })?;
    let path = stage.path().join("book.epub");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| io_error("creating staged EPUB", &path.display().to_string(), error))?;
    write(&mut original, &xhtml, &mut file)?;
    drop(file);
    publish_ready(stage, &mut original, &xhtml, destination)
}

fn publish_ready(
    stage: tempfile::TempDir,
    original: &mut ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
    destination: &Path,
) -> Result<(), MinamiError> {
    let path = stage.path().join("book.epub");
    let file = File::open(&path).map_err(|error| {
        io_error(
            "opening staged EPUB for verification",
            &path.display().to_string(),
            error,
        )
    })?;
    packaging::verify(original, xhtml, file)?;
    rename_new(&path, destination).map_err(|error| {
        io_error(
            "publishing EPUB without replacement",
            &destination.display().to_string(),
            error,
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::application::support::{self, Entry, Fixture, NCX, entries, entry, replace};

    const BODY: &str = "OEBPS/Text/Body.xhtml";
    const XHTML: &str =
        "<html xmlns='http://www.w3.org/1999/xhtml'><body><p>Hello.</p></body></html>";

    fn files() -> Vec<Entry> {
        let mut files = entries();
        for path in [
            BODY,
            "OEBPS/Text/Cover.xhtml",
            "OEBPS/Text/Illustrations.xhtml",
        ] {
            replace(&mut files, path, XHTML);
        }
        replace(
            &mut files,
            "OEBPS/toc.ncx",
            NCX.replace("#one", "")
                .replace("#two", "")
                .replace("#three", ""),
        );
        files.push(entry("last.bin", [0, 0xff, 0x80, 42]));
        files
    }

    fn prepared() -> (Fixture, tempfile::TempDir, PreparedTranslation) {
        let book = Fixture::new(files());
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        crate::extract_epub(&book.path, &work).unwrap();
        let prepared = prepare_reconstruction(
            &book.path,
            &work.join("control.json"),
            &[work.join("source.json")],
        )
        .unwrap();
        (book, root, prepared)
    }

    #[test]
    fn bld41_to_bld44_bld47_build_uses_only_owned_snapshot_and_approved_responses() {
        let (book, root, prepared) = prepared();
        let mut changed = files();
        replace(&mut changed, "last.bin", [10, 11, 12]);
        replace(
            &mut changed,
            BODY,
            XHTML.replace("Hello.", "Other edition."),
        );
        fs::remove_file(&book.path).unwrap();
        fs::write(&book.path, support::zip_bytes(changed)).unwrap();
        fs::write(
            root.path().join("work/control.json"),
            b"untrusted new control",
        )
        .unwrap();
        fs::write(
            root.path().join("work/source.json"),
            b"untrusted new response",
        )
        .unwrap();
        let output = root.path().join("out.epub");
        build_prepared(prepared, &output).unwrap();
        let mut archive = crate::epub::archive::open(&output).unwrap();
        assert_eq!(archive.read_xhtml(BODY).unwrap(), XHTML);
        let mut zip = archive.into_zip();
        let mut binary = Vec::new();
        std::io::Read::read_to_end(&mut zip.by_name("last.bin").unwrap(), &mut binary).unwrap();
        assert_eq!(binary, [0, 0xff, 0x80, 42]);
    }

    #[test]
    fn bld47_original_control_and_responses_may_disappear_after_preparation() {
        let (book, root, prepared) = prepared();
        drop(book);
        fs::remove_file(root.path().join("work/control.json")).unwrap();
        fs::remove_file(root.path().join("work/source.json")).unwrap();
        let output = root.path().join("out.epub");
        build_prepared(prepared, &output).unwrap();
        crate::inspect_epub(&output).unwrap();
    }

    #[test]
    fn bld53_independent_verification_rejects_corrupted_or_wrong_staged_resources() {
        for variant in 0..11 {
            let (_book, root, prepared) = prepared();
            let xhtml = reconstruct_prepared(&prepared)
                .unwrap()
                .into_iter()
                .map(|doc| (doc.path, doc.source))
                .collect();
            let mut original = prepared.into_archive().into_zip();
            let stage = tempfile::tempdir_in(root.path()).unwrap();
            let stage_path = stage.path().to_owned();
            let mut files = files();
            match variant {
                0 => {
                    files.pop();
                }
                1 => replace(&mut files, "last.bin", [0, 0xff, 0x80, 99]),
                2 => replace(&mut files, BODY, XHTML.replace("Hello.", "Unauthorized.")),
                3 => replace(&mut files, "OEBPS/content.opf", "<broken>"),
                4 => replace(&mut files, "OEBPS/toc.ncx", "<broken>"),
                5 => files.last_mut().unwrap().name = "unexpected.bin".into(),
                6 => files.retain(|entry| entry.name != BODY),
                7 => files.push(entry("unexpected.bin", [1])),
                _ => {}
            }
            let mut bytes = if variant == 8 {
                support::zip_bytes_with_mimetype_local_extra(files)
            } else {
                support::zip_bytes(files)
            };
            if variant == 9 {
                let offset = bytes
                    .windows(4)
                    .position(|bytes| bytes == [0, 0xff, 0x80, 42])
                    .unwrap();
                bytes[offset] ^= 1;
            }
            if variant == 10 {
                bytes.truncate(bytes.len() - 7);
            }
            fs::write(stage.path().join("book.epub"), bytes).unwrap();
            let output = root.path().join("out.epub");
            let sentinel = root.path().join("keep");
            fs::write(&sentinel, b"user data").unwrap();
            assert!(
                publish_ready(stage, &mut original, &xhtml, &output).is_err(),
                "variant {variant}"
            );
            assert!(!output.exists(), "variant {variant}");
            assert!(!stage_path.exists());
            assert_eq!(fs::read(sentinel).unwrap(), b"user data");
        }
    }

    #[test]
    fn bld54_destination_created_after_staging_is_never_replaced() {
        for directory in [false, true] {
            let (_book, root, prepared) = prepared();
            let xhtml = reconstruct_prepared(&prepared)
                .unwrap()
                .into_iter()
                .map(|doc| (doc.path, doc.source))
                .collect();
            let mut original = prepared.into_archive().into_zip();
            let stage = tempfile::tempdir_in(root.path()).unwrap();
            let stage_path = stage.path().to_owned();
            fs::write(stage.path().join("book.epub"), support::zip_bytes(files())).unwrap();
            let output = root.path().join("out.epub");
            if directory {
                fs::create_dir(&output).unwrap();
            } else {
                fs::write(&output, b"winner data").unwrap();
            }
            assert!(publish_ready(stage, &mut original, &xhtml, &output).is_err());
            if directory {
                assert_eq!(fs::read_dir(output).unwrap().count(), 0);
            } else {
                assert_eq!(fs::read(output).unwrap(), b"winner data");
            }
            assert!(!stage_path.exists());
        }
    }

    #[test]
    fn bld51_bld52_io_errors_during_resources_or_finalization_never_publish() {
        for (remaining, fail_central) in [(1000, false), (usize::MAX, true)] {
            let (_book, root, prepared) = prepared();
            let output = root.path().join("out.epub");
            let sentinel = root.path().join("keep");
            fs::write(&sentinel, b"user data").unwrap();
            let result = build_prepared_with(prepared, &output, |original, xhtml, file| {
                let mut failing = support::FailingWriter {
                    inner: file,
                    remaining,
                    fail_central,
                };
                packaging::write(original, xhtml, &mut failing)
            });
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("injected write failure")
            );
            assert!(!output.exists());
            assert_eq!(fs::read(&sentinel).unwrap(), b"user data");
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        }
    }
}
