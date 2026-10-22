# Architecture

## Technology Stack

Minami uses Rust 2024 and Cargo. `zip` reads the EPUB container, `roxmltree` validates XML, and `xmlparser` supplies lexical XHTML tokens. `serde` and `serde_json` serialize results, `clap` handles the CLI, and `thiserror` defines typed errors. `sha2` computes origin identity; `tempfile` owns snapshots and staging directories. Platform-specific bindings provide publication without replacement.

## Architectural Style

The implementation follows Data-Oriented Design: explicit data structures, small focused functions, and a clear flow of data, without object-oriented hierarchies. The CLI, application, and EPUB parser have separate responsibilities. Ordered results use vectors, parser errors are propagated explicitly, and invalid or ambiguous input fails with an error. Given the same EPUB, the report order is deterministic. Inspection is read-only and does not repair EPUB files.

## Modules

```text
main
└── cli
    └── application::{inspect, extract}
        └── epub
            ├── archive
            ├── package
            ├── navigation
            ├── xhtml
            └── segments

model and error provide shared contracts; x holds private X serialization types
```

- `main` starts the command-line application.
- `cli` parses `inspect`/`extract` arguments and presents results or errors; it does not parse XML or publish files itself.
- `application::inspect` coordinates the EPUB readers and assembles the report.
- `application::extract` snapshots the input, coordinates XHTML extraction and NCX section mapping, builds X data, and publishes the completed directory.
- `epub::archive` validates and reads ZIP entries without extracting them to disk.
- `epub::package` reads `container.xml`, locates the OPF, and parses its manifest and spine.
- `epub::navigation` parses the NCX `navMap` and validates its document targets.
- `epub::xhtml` records exact source spans, decodes text, and validates XML, namespaces, IDs, declarations, and limits.
- `epub::segments` groups body content and emits protected markers, preserved spans, and normalized section-boundary positions.
- `model` defines the public report structures; `error` defines errors shared by the library and CLI.
- `x` contains private Serde records matching the X contract; these do not replace the inspection model.

## Data Flow

```text
EPUB
  ↓
ZIP container and mimetype
  ↓
container.xml
  ↓
OPF manifest and spine
  ↓
NCX navigation
  ↓
InspectionReport
  ↓
JSON
```

The application builds the full report before the CLI serializes it. A failed inspection returns an error instead of a partial report.

Extraction reuses the same ZIP/package/navigation readers:

```text
original EPUB → bounded private snapshot + SHA-256
             → package/spine/NCX → manifest XHTML in deterministic order
             → lexical spans → flow groups/markers → sections and X models
             → complete JSON buffers → sibling staging directory → new output directory
```

The snapshot is an automatically removed temporary file, not an extracted resource tree or persistent cache. Hashing and parsing use the same copied bytes, even if the original changes afterward. Observable source size/modification-time changes during copying cause failure. The serialized files contain no temporary paths or timestamps.

XHTML tokenization operates on the original UTF-8 buffer. Only authorized `&nbsp;` references in lexical text/attribute contexts are changed to the equal-length `&#160;` in a validation copy. Comments and CDATA are untouched by that substitution. Text decoding handles numeric references and XML line endings separately, retaining original spans, including exact protected-space locations. No full-document serialization is used to recover offsets.

Publishing uses a private temporary directory under the existing output parent. Both complete JSON files are written there before one native no-replace rename exposes the result. Windows uses [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) without replacement/copy flags; Linux/Android use [renameat_with](https://docs.rs/rustix/latest/rustix/fs/fn.renameat_with.html) with `NOREPLACE`. Existing files, directories, and symlinks are not replaced, including a destination appearing after preflight. Concurrent publishers cannot combine artifacts. Failed staging/publication drops only the operation's temporary resources. Abrupt termination may leave staging data; crash durability is outside this milestone.

## Data Contracts

- `InspectionReport` contains the EPUB version, package path, manifest resources, spine order, and navigation entries.
- `ManifestResource` identifies a resource by manifest ID, normalized internal path, and media type.
- `SpineEntry` preserves each `idref` and its `linear` value in declared reading order.
- `NavigationEntry` stores an optional NCX ID, title, document path, optional fragment, preorder position, and nesting depth.
- `MinamiError` distinguishes I/O, ZIP, resource-limit, XML, missing-structure, invalid-reference, ambiguity, and unsupported-format failures.

Manifest and spine order are preserved from the OPF. Navigation order follows document order in `navMap`, independent of `playOrder`. A document may have multiple navigation entries; a missing fragment remains absent.

`extract_epub(path: &Path, output: &Path) -> Result<(), MinamiError>` is the additional public application operation. It publishes `control.json` and `source.json` according to the normative [X format contract](x-format.md). X records remain private; translated-response validation and EPUB reconstruction remain future work. Inspection retains its existing contract.

## Supported EPUB Profile

Both operations support EPUB 2.0 packages with an OPF and NCX 2005-1 navigation. Reading order comes from the spine; primary navigation comes from `navMap`. Inspection preserves fragment references without resolving XHTML IDs. Extraction additionally resolves section targets and rejects missing, empty, opaque-descendant, or mid-flow targets that cannot form permitted boundaries.

Extraction covers each manifest XHTML once: unique spine documents first (including `linear="no"`), then remaining manifest XHTML. NCX aliases share normalized section boundaries; unindexed prefixes and documents remain represented. Flow groups keep mixed paragraphs together and separate nested blocks/cells. Tags, attributes, SVG, callouts, and special spaces use original spans rather than editable XHTML. The UTF-8 vocabulary and offline entity subset are defined by X 1.0; attributes and head content are not translated.

The parser rejects duplicate or unsafe paths, references that escape the EPUB root, encrypted or unsupported ZIP entries, and unsupported structural formats. ZIP and XML reads have explicit size and count limits. XML parsing does not resolve external entities or access the network. Resource bytes are checked as they are read; auxiliary manifest assets are checked for existence but are not fully inspected.

Beyond the existing ZIP limits, extraction bounds each XHTML to 32 MiB, 100,000 nodes/text chunks/attributes, 256 attributes per element, and 128 XML nesting levels. Paired inline nesting is limited to 64 levels. The extraction map permits 200,000 document/section/segment/marker/part/preserved records and 64 MiB of editable text; each serialized JSON artifact is capped at 64 MiB, checked before appending to its output buffer. Limit violations fail explicitly without truncation.

## Limitations

- EPUB 3 is not supported.
- XHTML extraction follows the explicit X vocabulary; unknown content is rejected. Computed CSS, layout, OCR, and linguistic meaning are not interpreted.
- NCX `pageList` and `navList` are not included in the report.
- Full independent extraction-coverage validation and translated-response validation are not implemented. Textual attributes, head/OPF metadata, and NCX labels remain unchanged.
- EPUB reconstruction and LLM integration are not implemented.
