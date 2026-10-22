use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

use zip::{CompressionMethod, ZipArchive};

use super::{ambiguous, invalid, missing, unsupported};
use crate::MinamiError;

const MAX_ENTRIES: usize = 4096;
pub(crate) const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_XML_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 1024;

pub(crate) struct EpubArchive {
    zip: ZipArchive<File>,
    files: BTreeMap<String, usize>,
    verified: BTreeSet<String>,
}

fn zip_error(
    resource: &str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> MinamiError {
    MinamiError::InvalidZip {
        resource: resource.into(),
        source: Box::new(source),
    }
}

fn limit(resource: &str, limit: &'static str) -> MinamiError {
    MinamiError::LimitExceeded {
        resource: resource.into(),
        limit,
    }
}

// Bound ZIP metadata before zip allocates its central-directory index.
fn preflight(file: &mut File, resource: &str, size: u64) -> Result<(usize, u64), MinamiError> {
    if size > MAX_ARCHIVE_BYTES {
        return Err(limit(resource, "128 MiB archive"));
    }
    let tail_size = size.min(65_557);
    file.seek(SeekFrom::End(-(tail_size as i64)))
        .map_err(|e| zip_error(resource, e))?;
    let mut tail = vec![0; tail_size as usize];
    file.read_exact(&mut tail)
        .map_err(|e| zip_error(resource, e))?;
    let end = tail
        .windows(22)
        .enumerate()
        .rfind(|(i, w)| {
            &w[..4] == b"PK\x05\x06"
                && i + 22 + usize::from(u16::from_le_bytes([w[20], w[21]])) == tail.len()
        })
        .map(|(i, _)| i)
        .ok_or_else(|| {
            zip_error(
                resource,
                io::Error::new(io::ErrorKind::InvalidData, "missing ZIP end record"),
            )
        })?;
    let record = &tail[end..];
    let u16_at = |i| u16::from_le_bytes([record[i], record[i + 1]]);
    let u32_at = |i| u32::from_le_bytes([record[i], record[i + 1], record[i + 2], record[i + 3]]);
    if u16_at(4) != 0 || u16_at(6) != 0 || u16_at(8) != u16_at(10) {
        return Err(unsupported(resource, "multi-disk ZIP"));
    }
    if u16_at(10) == u16::MAX || u32_at(12) == u32::MAX || u32_at(16) == u32::MAX {
        return Err(unsupported(resource, "ZIP64"));
    }
    if usize::from(u16_at(10)) > MAX_ENTRIES {
        return Err(limit(resource, "4096 ZIP entries"));
    }
    if u32_at(12) > 8 * 1024 * 1024 {
        return Err(limit(resource, "8 MiB ZIP central directory"));
    }
    let directory_start = u64::from(u32_at(16));
    if directory_start + u64::from(u32_at(12)) != size - tail_size + end as u64 {
        return Err(zip_error(
            resource,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "inconsistent ZIP central-directory bounds",
            ),
        ));
    }
    file.rewind().map_err(|e| zip_error(resource, e))?;
    Ok((usize::from(u16_at(10)), directory_start))
}

pub(crate) fn open(path: &Path) -> Result<EpubArchive, MinamiError> {
    let resource = path.display().to_string();
    let file = File::open(path).map_err(|source| MinamiError::Io {
        operation: "opening EPUB",
        resource: resource.clone(),
        source,
    })?;
    from_file(file, &resource)
}

pub(crate) fn from_file(mut file: File, resource: &str) -> Result<EpubArchive, MinamiError> {
    let resource = resource.to_owned();
    let metadata = file.metadata().map_err(|source| MinamiError::Io {
        operation: "reading EPUB metadata",
        resource: resource.clone(),
        source,
    })?;
    let (entry_count, directory_start) = preflight(&mut file, &resource, metadata.len())?;
    let mut zip = ZipArchive::new(file).map_err(|e| zip_error(&resource, e))?;
    if zip.len() != entry_count
        || zip.central_directory_start() != directory_start
        || zip.offset() != 0
    {
        return Err(zip_error(
            &resource,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ambiguous ZIP directory or duplicate entries",
            ),
        ));
    }
    if zip.len() > MAX_ENTRIES {
        return Err(limit(&resource, "4096 ZIP entries"));
    }
    let mut names = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| zip_error(&resource, e))?;
        let name = entry
            .name()
            .map_err(|e| zip_error(&resource, e))?
            .into_owned();
        let normalized = name.strip_suffix('/').unwrap_or(&name);
        validate_entry_name(normalized)?;
        if !names.insert(normalized.to_string()) {
            return Err(ambiguous(
                &resource,
                format!("duplicate ZIP path {normalized}"),
            ));
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(limit(&name, "32 MiB per ZIP entry"));
        }
        total += entry.size();
        if total > MAX_TOTAL_BYTES {
            return Err(limit(&resource, "256 MiB total uncompressed bytes"));
        }
        if entry.encrypted() || entry.is_symlink() {
            return Err(unsupported(&name, "encrypted ZIP entry or symbolic link"));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(unsupported(
                &name,
                format!("ZIP compression {:?}", entry.compression()),
            ));
        }
        if !entry.is_dir() {
            files.insert(name, i);
        }
    }
    zip.validate_header_consistency()
        .map_err(|e| zip_error(&resource, e))?;
    if files.contains_key("META-INF/encryption.xml") {
        return Err(unsupported("META-INF/encryption.xml", "EPUB encryption"));
    }
    let mime_index = *files
        .get("mimetype")
        .ok_or_else(|| missing(&resource, "mimetype"))?;
    let mime = zip
        .by_index_raw(mime_index)
        .map_err(|e| zip_error("mimetype", e))?;
    if mime.header_start() != 0 || mime.compression() != CompressionMethod::Stored {
        return Err(invalid(
            "mimetype",
            "must be the first physical entry, uncompressed",
        ));
    }
    let local_extra_len = mime
        .data_start()
        .and_then(|data_start| {
            mime.header_start()
                .checked_add(30 + mime.name_raw().len() as u64)
                .and_then(|header_end| data_start.checked_sub(header_end))
        })
        .ok_or_else(|| {
            zip_error(
                "mimetype",
                io::Error::new(io::ErrorKind::InvalidData, "invalid local header bounds"),
            )
        })?;
    if local_extra_len != 0 {
        return Err(zip_error(
            "mimetype",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "local header contains extra fields",
            ),
        ));
    }
    drop(mime);
    let mut archive = EpubArchive {
        zip,
        files,
        verified: BTreeSet::new(),
    };
    if archive.read_xml("mimetype")? != "application/epub+zip" {
        return Err(invalid("mimetype", "expected exactly application/epub+zip"));
    }
    Ok(archive)
}

fn validate_entry_name(name: &str) -> Result<(), MinamiError> {
    if name.len() > MAX_PATH_BYTES {
        return Err(limit(name, "1024 bytes per internal path"));
    }
    if name.is_empty()
        || name.contains(['\\', ':', '?', '#'])
        || name.chars().any(char::is_control)
        || name.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return Err(invalid(name, "unsafe or non-normalized ZIP entry path"));
    }
    Ok(())
}

impl EpubArchive {
    pub(crate) fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    pub(crate) fn read_xml(&mut self, path: &str) -> Result<String, MinamiError> {
        self.read_utf8(path, MAX_XML_BYTES, "4 MiB per structural XML")
    }

    pub(crate) fn read_xhtml(&mut self, path: &str) -> Result<String, MinamiError> {
        self.read_utf8(path, MAX_ENTRY_BYTES, "32 MiB per XHTML")
    }

    fn read_utf8(
        &mut self,
        path: &str,
        max: u64,
        description: &'static str,
    ) -> Result<String, MinamiError> {
        let index = *self
            .files
            .get(path)
            .ok_or_else(|| missing(path, "ZIP entry"))?;
        let mut entry = self.zip.by_index(index).map_err(|e| zip_error(path, e))?;
        if entry.size() > max {
            return Err(limit(path, description));
        }
        let mut bytes = Vec::new();
        (&mut entry)
            .take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| zip_error(path, e))?;
        if bytes.len() as u64 > max {
            return Err(limit(path, description));
        }
        if bytes.len() as u64 != entry.size() {
            return Err(zip_error(
                path,
                io::Error::new(io::ErrorKind::InvalidData, "uncompressed size mismatch"),
            ));
        }
        self.verified.insert(path.into());
        String::from_utf8(bytes).map_err(|source| MinamiError::MalformedXml {
            resource: path.into(),
            source: Box::new(source),
        })
    }

    pub(crate) fn verify(&mut self, path: &str) -> Result<(), MinamiError> {
        if self.verified.contains(path) {
            return Ok(());
        }
        let index = *self
            .files
            .get(path)
            .ok_or_else(|| invalid(path, "missing ZIP entry"))?;
        let mut entry = self.zip.by_index(index).map_err(|e| zip_error(path, e))?;
        let count = io::copy(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut io::sink())
            .map_err(|e| zip_error(path, e))?;
        if count > MAX_ENTRY_BYTES {
            return Err(limit(path, "32 MiB per ZIP entry"));
        }
        if count != entry.size() {
            return Err(zip_error(
                path,
                io::Error::new(io::ErrorKind::InvalidData, "uncompressed size mismatch"),
            ));
        }
        self.verified.insert(path.into());
        Ok(())
    }
}

fn decode_uri(value: &str, resource: &str, path: bool) -> Result<String, MinamiError> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            result.push(b);
            continue;
        }
        let hi = bytes.next().and_then(|b| (b as char).to_digit(16));
        let lo = bytes.next().and_then(|b| (b as char).to_digit(16));
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return Err(invalid(resource, format!("invalid URI escape in {value}")));
        };
        let decoded = (hi * 16 + lo) as u8;
        if path && matches!(decoded, b'/' | b'\\') {
            return Err(invalid(
                resource,
                format!("encoded path separator in {value}"),
            ));
        }
        result.push(decoded);
    }
    let decoded = String::from_utf8(result).map_err(|_| invalid(resource, "URI is not UTF-8"))?;
    if decoded.chars().any(char::is_control) {
        return Err(invalid(resource, "control character in URI"));
    }
    Ok(decoded)
}

pub(crate) fn resolve(
    base: &str,
    reference: &str,
) -> Result<(String, Option<String>), MinamiError> {
    if reference.len() > MAX_PATH_BYTES {
        return Err(limit(base, "1024 bytes per reference"));
    }
    let (path, fragment) = match reference.split_once('#') {
        Some((path, fragment)) => (path, Some(decode_uri(fragment, base, false)?)),
        None => (reference, None),
    };
    let path = decode_uri(path, base, true)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', ':', '?', '#'])
        || reference.contains('?')
    {
        return Err(invalid(
            base,
            format!("expected a local document reference: {reference}"),
        ));
    }
    let parent = base.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut parts: Vec<&str> = parent.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid(
                        base,
                        format!("reference escapes EPUB root: {reference}"),
                    ));
                }
            }
            "" => return Err(invalid(base, format!("empty path segment: {reference}"))),
            _ => parts.push(part),
        }
    }
    let resolved = parts.join("/");
    validate_entry_name(&resolved)?;
    Ok((resolved, fragment))
}
