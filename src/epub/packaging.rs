use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

use sha2::{Digest, Sha256};
use zip::{ZipArchive, ZipWriter};

use super::{
    archive::{self, MAX_ARCHIVE_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_TOTAL_BYTES, zip_error},
    invalid, navigation, package,
};
use crate::MinamiError;

pub(crate) fn write(
    original: &mut ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
    output: &mut (impl Write + Seek),
) -> Result<(), MinamiError> {
    write_with_budget(original, xhtml, output, MAX_ARCHIVE_BYTES)
}

fn write_with_budget(
    original: &mut ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
    output: &mut (impl Write + Seek),
    max_bytes: u64,
) -> Result<(), MinamiError> {
    check_sizes(original, xhtml)?;
    let mut bounded = BoundedWriter {
        inner: output,
        position: 0,
        max: max_bytes,
        exceeded: false,
    };
    let result = write_entries(original, xhtml, &mut bounded);
    if bounded.exceeded {
        return Err(limit("staged EPUB", "128 MiB output ZIP"));
    }
    result
}

fn check_sizes(
    original: &ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
) -> Result<(), MinamiError> {
    if original.len() > MAX_ENTRIES {
        return Err(limit("staged EPUB", "4096 ZIP entries"));
    }
    for path in xhtml.keys() {
        if original.index_for_name(path).is_none() {
            return Err(invalid(path, "rebuilt XHTML has no original ZIP resource"));
        }
    }
    let mut total = 0u64;
    for index in 0..original.len() {
        let entry = original
            .by_index_data(index)
            .map_err(|error| zip_error("snapshot", error))?;
        let name = entry.name().map_err(|error| zip_error("snapshot", error))?;
        let size = xhtml
            .get(name.as_ref())
            .map_or(entry.size(), |source| source.len() as u64);
        add_size(&mut total, size, &name)?;
    }
    Ok(())
}

fn add_size(total: &mut u64, size: u64, path: &str) -> Result<(), MinamiError> {
    if size > MAX_ENTRY_BYTES {
        return Err(limit(path, "32 MiB per output ZIP entry"));
    }
    *total = total
        .checked_add(size)
        .ok_or_else(|| limit(path, "256 MiB total output bytes"))?;
    if *total > MAX_TOTAL_BYTES {
        return Err(limit(path, "256 MiB total output bytes"));
    }
    Ok(())
}

fn write_entries(
    original: &mut ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
    output: &mut (impl Write + Seek),
) -> Result<(), MinamiError> {
    let mut writer = ZipWriter::new(output);
    writer
        .set_raw_comment(original.comment().into())
        .map_err(|error| zip_error("staged EPUB comment", error))?;
    let mime = original
        .index_for_name("mimetype")
        .ok_or_else(|| invalid("snapshot", "missing mimetype"))?;
    // The central directory may list mimetype later even when its physical header is first.
    for index in std::iter::once(mime).chain((0..original.len()).filter(|index| *index != mime)) {
        let mut entry = original
            .by_index(index)
            .map_err(|error| zip_error("snapshot", error))?;
        let name = entry
            .name()
            .map_err(|error| zip_error("snapshot", error))?
            .into_owned();
        let mut options = entry
            .options()
            .large_file(false)
            .last_modified_time(
                entry
                    .last_modified()
                    .filter(|time| time.is_valid())
                    .unwrap_or_default(),
            )
            .into_full_options();
        // Legacy encodings can expand beyond ZIP's UTF-8 comment field capacity.
        if entry.comment().len() <= usize::from(u16::MAX) {
            options = options.with_file_comment(entry.comment());
        }
        // start_file preserves even directory records with payloads accepted by the reader.
        writer
            .start_file(&name, options)
            .map_err(|error| zip_error(&name, error))?;
        if let Some(source) = xhtml.get(&name) {
            writer
                .write_all(source.as_bytes())
                .map_err(|source| write_error(&name, source))?;
        } else {
            let size = entry.size();
            read_entry(&mut entry, size, &name, |bytes| {
                writer
                    .write_all(bytes)
                    .map_err(|source| write_error(&name, source))
            })?;
        }
    }
    writer
        .finish()
        .map_err(|error| zip_error("finishing staged EPUB", error))?;
    Ok(())
}

/// Read the completed file independently; no fingerprints from the writer are trusted.
pub(crate) fn verify(
    original: &mut ZipArchive<File>,
    xhtml: &BTreeMap<String, String>,
    file: File,
) -> Result<(), MinamiError> {
    let mut rebuilt = archive::from_file(file, "staged EPUB verification")?;
    let package_path = package::locate(&mut rebuilt)?;
    let package = package::parse(&mut rebuilt, &package_path)?;
    navigation::parse(&mut rebuilt, &package.ncx_path, &package.resources)?;
    let mut rebuilt = rebuilt.into_zip();
    if original.len() != rebuilt.len() {
        return Err(invalid(
            "staged EPUB verification",
            "ZIP resource set changed",
        ));
    }
    for index in 0..original.len() {
        let mut source = original
            .by_index(index)
            .map_err(|error| zip_error("snapshot verification", error))?;
        let path = source
            .name()
            .map_err(|error| zip_error("snapshot verification", error))?
            .into_owned();
        let mut actual = rebuilt
            .by_name(&path)
            .map_err(|error| zip_error(&path, error))?;
        if source.is_dir() != actual.is_dir() || source.compression() != actual.compression() {
            return Err(invalid(
                &path,
                "staged EPUB changed ZIP resource type or compression",
            ));
        }
        let expected = match xhtml.get(&path) {
            Some(approved) => Sha256::digest(approved.as_bytes()).into(),
            None => fingerprint(&mut source, &path)?,
        };
        if fingerprint(&mut actual, &path)? != expected {
            return Err(invalid(
                &path,
                "staged EPUB bytes differ from the snapshot or approved XHTML",
            ));
        }
    }
    Ok(())
}

fn fingerprint(
    entry: &mut zip::read::ZipFile<'_, File>,
    path: &str,
) -> Result<[u8; 32], MinamiError> {
    let size = entry.size();
    let mut hash = Sha256::new();
    read_entry(entry, size, path, |bytes| {
        hash.update(bytes);
        Ok(())
    })?;
    Ok(hash.finalize().into())
}

fn read_entry(
    reader: &mut impl Read,
    size: u64,
    path: &str,
    mut consume: impl FnMut(&[u8]) -> Result<(), MinamiError>,
) -> Result<(), MinamiError> {
    if size > MAX_ENTRY_BYTES {
        return Err(limit(path, "32 MiB per ZIP entry"));
    }
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result.map_err(|error| zip_error(path, error))?,
        };
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| limit(path, "32 MiB per ZIP entry"))?;
        if total > size {
            return Err(invalid(path, "ZIP resource exceeds its declared size"));
        }
        consume(&buffer[..n])?;
    }
    if total != size {
        return Err(invalid(
            path,
            "truncated ZIP resource: uncompressed size mismatch",
        ));
    }
    Ok(())
}

struct BoundedWriter<W> {
    inner: W,
    position: u64,
    max: u64,
    exceeded: bool,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.max.saturating_sub(self.position) {
            self.exceeded = true;
            return Err(io::Error::other("output size limit exceeded"));
        }
        let n = self.inner.write(bytes)?;
        self.position += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for BoundedWriter<W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = self.inner.seek(from)?;
        if position > self.max {
            self.exceeded = true;
            return Err(io::Error::other("output size limit exceeded"));
        }
        self.position = position;
        Ok(position)
    }
}

fn limit(resource: &str, limit: &'static str) -> MinamiError {
    MinamiError::LimitExceeded {
        resource: resource.into(),
        limit,
    }
}

fn write_error(resource: &str, source: io::Error) -> MinamiError {
    MinamiError::Io {
        operation: "writing staged EPUB resource",
        resource: resource.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::application::support::{FailingWriter, Fixture, entry};

    #[test]
    fn bld59_large_binary_resource_is_streamed_with_identical_bytes() {
        let fixture = Fixture::new(vec![
            entry("mimetype", b"application/epub+zip"),
            entry("binary", vec![0xff; 8 * 1024 * 1024]),
        ]);
        let mut original = archive::open(&fixture.path).unwrap().into_zip();
        let mut output = Cursor::new(Vec::new());
        write(&mut original, &BTreeMap::new(), &mut output).unwrap();
        let mut rebuilt = ZipArchive::new(Cursor::new(output.into_inner())).unwrap();
        assert_eq!(
            fingerprint(&mut original.by_name("binary").unwrap(), "binary").unwrap(),
            {
                let mut hash = Sha256::new();
                let mut binary = rebuilt.by_name("binary").unwrap();
                read_entry(&mut binary, 8 * 1024 * 1024, "binary", |bytes| {
                    hash.update(bytes);
                    Ok(())
                })
                .unwrap();
                <[u8; 32]>::from(hash.finalize())
            }
        );
    }

    #[test]
    fn bld61_physical_zip_budget_covers_writes_seeks_and_final_directory() {
        let mut cursor = Cursor::new(Vec::new());
        let mut bounded = BoundedWriter {
            inner: &mut cursor,
            position: 0,
            max: 16,
            exceeded: false,
        };
        bounded.write_all(&[1; 16]).unwrap();
        bounded.seek(SeekFrom::Start(0)).unwrap();
        bounded.write_all(&[2; 16]).unwrap();
        assert!(bounded.write_all(&[3]).is_err());
        assert!(bounded.exceeded);
        assert_eq!(bounded.inner.get_ref().len(), 16);
        let mut cursor = Cursor::new(Vec::new());
        let mut bounded = BoundedWriter {
            inner: &mut cursor,
            position: 0,
            max: 64,
            exceeded: false,
        };
        let mut writer = ZipWriter::new(&mut bounded);
        writer
            .start_file(
                "mimetype",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        assert!(writer.finish().is_err());
        assert!(bounded.exceeded);
        assert!(bounded.inner.get_ref().len() <= 64);

        let fixture = Fixture::new(vec![entry("mimetype", b"application/epub+zip")]);
        let mut original = archive::open(&fixture.path).unwrap().into_zip();
        let mut output = Cursor::new(Vec::new());
        assert!(matches!(
            write_with_budget(&mut original, &BTreeMap::new(), &mut output, 64),
            Err(MinamiError::LimitExceeded { .. })
        ));
        assert!(output.get_ref().len() <= 64);
    }

    #[test]
    fn bld62_bld69_output_size_arithmetic_is_checked_before_copying() {
        let mut total = MAX_TOTAL_BYTES - MAX_ENTRY_BYTES;
        add_size(&mut total, MAX_ENTRY_BYTES, "exact").unwrap();
        assert_eq!(total, MAX_TOTAL_BYTES);
        assert!(add_size(&mut total, 1, "over total").is_err());
        let mut total = 0;
        assert!(add_size(&mut total, MAX_ENTRY_BYTES + 1, "over entry").is_err());
        let mut total = u64::MAX;
        assert!(add_size(&mut total, 1, "overflow").is_err());
    }

    #[test]
    fn bld60_total_output_expansion_is_rejected_before_the_first_write() {
        let mut files = vec![
            entry("mimetype", b"application/epub+zip"),
            entry("text.xhtml", b"x"),
        ];
        for index in 0..8 {
            files.push(entry(&format!("binary{index}"), []));
        }
        let mut bytes = crate::application::support::zip_bytes(files);
        for index in 0..8 {
            let name = format!("binary{index}");
            crate::application::support::declared_size(
                &mut bytes,
                &name,
                if index == 7 {
                    16 * 1024 * 1024
                } else {
                    32 * 1024 * 1024
                },
            );
        }
        let fixture = Fixture::bytes(&bytes);
        let mut original = ZipArchive::new(File::open(&fixture.path).unwrap()).unwrap();
        let xhtml = BTreeMap::from([("text.xhtml".into(), "x".repeat(17 * 1024 * 1024))]);
        let mut output = Cursor::new(Vec::new());
        assert!(matches!(
            write(&mut original, &xhtml, &mut output),
            Err(MinamiError::LimitExceeded { .. })
        ));
        assert!(output.into_inner().is_empty());
    }

    #[test]
    fn declared_entry_size_is_checked_without_truncating_or_allocating_it() {
        let mut bytes = Cursor::new(b"too short");
        assert!(read_entry(&mut bytes, 10, "binary", |_| Ok(())).is_err());
        let mut bytes = Cursor::new(b"too long");
        assert!(
            read_entry(&mut bytes, 2, "binary", |_| panic!(
                "must reject before consuming oversized content"
            ))
            .is_err()
        );
        let mut bytes = Cursor::new(b"ignored");
        assert!(
            read_entry(&mut bytes, MAX_ENTRY_BYTES + 1, "binary", |_| panic!(
                "must reject before reading"
            ))
            .is_err()
        );
        assert_eq!(bytes.position(), 0);
    }

    #[test]
    fn bld51_bld52_write_and_zip_finalization_errors_are_propagated() {
        let fixture = Fixture::new(vec![
            entry("mimetype", b"application/epub+zip"),
            entry("last.bin", [0xff; 1024]),
        ]);
        for (remaining, fail_central) in [(500, false), (usize::MAX, true)] {
            let mut original = archive::open(&fixture.path).unwrap().into_zip();
            let mut output = FailingWriter {
                inner: Cursor::new(Vec::new()),
                remaining,
                fail_central,
            };
            let error = write(&mut original, &BTreeMap::new(), &mut output).unwrap_err();
            assert!(
                error.to_string().contains("injected write failure"),
                "{error}"
            );
            assert!(!output.inner.get_ref().is_empty());
        }
    }

    struct InterruptedOnce(bool);
    impl Read for InterruptedOnce {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.0, false) {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                Ok(0)
            }
        }
    }
    #[test]
    fn interrupted_reads_retry_and_other_read_errors_propagate() {
        read_entry(&mut InterruptedOnce(true), 0, "binary", |_| Ok(())).unwrap();
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("injected read failure"))
            }
        }
        let error = read_entry(&mut Broken, 0, "binary", |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("injected read failure"));
    }
}
