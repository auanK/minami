use std::{
    fs::{self, OpenOptions},
    io::{self, Cursor, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use zip::{
    CompressionMethod, ZipWriter,
    write::{FullFileOptions, SimpleFileOptions},
};

pub const CONTAINER: &str = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
 <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
pub const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="book">
 <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="book">test</dc:identifier></metadata>
 <manifest>
  <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
  <item id="body" href="Text/Body.xhtml" media-type="application/xhtml+xml"/>
  <item id="cover" href="Text/Cover.xhtml" media-type="application/xhtml+xml"/>
  <item id="illustrations" href="Text/Illustrations.xhtml" media-type="application/xhtml+xml"/>
 </manifest>
 <spine toc="ncx"><itemref idref="cover"/><itemref idref="illustrations" linear="no"/><itemref idref="body"/></spine>
</package>"#;
pub const NCX: &str = r#"<?xml version="1.0"?>
<!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
 <navMap>
  <navPoint id="one" playOrder="1"><navLabel><text>First</text></navLabel><content src="Text/Body.xhtml#one"/>
   <navPoint id="two" playOrder="2"><navLabel><text>Second</text></navLabel><content src="Text/Body.xhtml#two"/></navPoint>
  </navPoint>
  <navPoint playOrder="3"><navLabel><text>Third</text></navLabel><content src="Text/Body.xhtml#three"/></navPoint>
 </navMap>
</ncx>"#;

pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
    pub compression: CompressionMethod,
}

pub fn entry(name: &str, data: impl AsRef<[u8]>) -> Entry {
    Entry {
        name: name.into(),
        data: data.as_ref().into(),
        compression: CompressionMethod::Stored,
    }
}

pub fn entries() -> Vec<Entry> {
    vec![
        entry("mimetype", "application/epub+zip"),
        entry("META-INF/container.xml", CONTAINER),
        entry("OEBPS/content.opf", OPF),
        entry("OEBPS/toc.ncx", NCX),
        entry(
            "OEBPS/Text/Body.xhtml",
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body/></html>",
        ),
        entry("OEBPS/Text/Illustrations.xhtml", "<html/>"),
        entry("OEBPS/Text/Cover.xhtml", "<html/>"),
    ]
}

pub fn replace(entries: &mut [Entry], name: &str, data: impl AsRef<[u8]>) {
    entries.iter_mut().find(|e| e.name == name).unwrap().data = data.as_ref().into();
}

pub fn zip_bytes(entries: Vec<Entry>) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for e in entries {
        writer
            .start_file(
                e.name,
                SimpleFileOptions::default().compression_method(e.compression),
            )
            .unwrap();
        writer.write_all(&e.data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

pub fn zip_bytes_with_mimetype_local_extra(entries: Vec<Entry>) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for e in entries {
        let mut options = FullFileOptions::default().compression_method(e.compression);
        if e.name == "mimetype" {
            options
                .add_extra_field(0x5455, [1, 0, 0, 0, 0], false)
                .unwrap();
        }
        writer.start_file(e.name, options).unwrap();
        writer.write_all(&e.data).unwrap();
    }
    let mut bytes = writer.finish().unwrap().into_inner();
    let end = bytes
        .windows(22)
        .enumerate()
        .rfind(|(i, w)| {
            &w[..4] == b"PK\x05\x06"
                && i + 22 + usize::from(u16::from_le_bytes([w[20], w[21]])) == bytes.len()
        })
        .map(|(i, _)| i)
        .unwrap();
    let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let name_len = usize::from(u16::from_le_bytes([
        bytes[central + 28],
        bytes[central + 29],
    ]));
    let extra_len = usize::from(u16::from_le_bytes([
        bytes[central + 30],
        bytes[central + 31],
    ]));
    assert_eq!(&bytes[central + 46..central + 46 + name_len], b"mimetype");
    assert_eq!(extra_len, 9);
    bytes.drain(central + 46 + name_len..central + 46 + name_len + extra_len);
    bytes[central + 30..central + 32].copy_from_slice(&0u16.to_le_bytes());
    let end = end - extra_len;
    let directory_size = u32::from_le_bytes(bytes[end + 12..end + 16].try_into().unwrap());
    bytes[end + 12..end + 16].copy_from_slice(&(directory_size - extra_len as u32).to_le_bytes());
    bytes
}

pub struct Fixture {
    pub path: PathBuf,
}

impl Fixture {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self::bytes(&zip_bytes(entries))
    }

    pub fn bytes(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "minami-{}-{}.epub",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        Self { path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn patch_name(bytes: &mut [u8], from: &str, to: &str) {
    assert_eq!(from.len(), to.len());
    let positions: Vec<_> = bytes
        .windows(from.len())
        .enumerate()
        .filter_map(|(i, w)| (w == from.as_bytes()).then_some(i))
        .collect();
    assert_eq!(positions.len(), 2);
    for i in positions {
        bytes[i..i + to.len()].copy_from_slice(to.as_bytes());
    }
}

pub fn declared_size(bytes: &mut [u8], name: &str, size: u32) {
    let signature = b"PK\x01\x02";
    let i = bytes
        .windows(4)
        .enumerate()
        .find_map(|(i, w)| {
            (w == signature && bytes.get(i + 46..i + 46 + name.len()) == Some(name.as_bytes()))
                .then_some(i)
        })
        .unwrap();
    bytes[i + 24..i + 28].copy_from_slice(&size.to_le_bytes());
}

#[allow(dead_code)]
pub struct FailingWriter<W> {
    pub inner: W,
    pub remaining: usize,
    pub fail_central: bool,
}

impl<W: Write> Write for FailingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 || (self.fail_central && bytes.starts_with(b"PK\x01\x02")) {
            return Err(io::Error::other("injected write failure"));
        }
        let n = bytes.len().min(self.remaining);
        self.remaining -= n;
        self.inner.write(&bytes[..n])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for FailingWriter<W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.inner.seek(from)
    }
}
