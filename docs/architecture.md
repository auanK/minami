# Architecture

## Technology Stack

Minami uses Rust 2024 and Cargo. `zip` reads the EPUB container, `roxmltree` validates XML, and `xmlparser` supplies lexical XHTML tokens. `serde` and `serde_json` serialize results, `clap` handles the CLI, and `thiserror` defines typed errors. `sha2` computes origin identity; `tempfile` owns snapshots and staging directories. Platform-specific bindings provide publication without replacement.

## Architectural Style

The implementation follows Data-Oriented Design: explicit data structures, small focused functions, and a clear flow of data, without object-oriented hierarchies. The CLI, application, and EPUB parser have separate responsibilities. Ordered results use vectors, parser errors are propagated explicitly, and invalid or ambiguous input fails with an error. Given the same EPUB, the report order is deterministic. Inspection is read-only and does not repair EPUB files.

## Modules

```text
main
└── cli
    └── application::{inspect, extract, validate, reconstruct, audit, batching}
        └── epub
            ├── archive
            ├── package
            ├── navigation
            ├── xhtml
            └── segments

model and error provide shared contracts; x holds private X serialization types
```

- `main` starts the command-line application.
- `cli` parses `inspect`/`extract`/`validate` arguments and presents results or errors; it does not parse XML or publish files itself.
- `application::inspect` coordinates the EPUB readers and assembles the report.
- `application::extract` snapshots the input, coordinates XHTML extraction and NCX section mapping, builds X data, and publishes the completed directory.
- `application::audit` independently checks generated text, tokens, preserved spans, sections, and manifest coverage against each original lexical XHTML tree before serialization or publication. It also derives immutable validation facts: XHTML ID anchors and promoted NCX boundary markers with their ancestor chains.
- `application::validate` regenerates the extraction from the same private snapshot, strictly parses the supplied control and responses, compares the complete control map, and checks coverage, markers, atoms, XHTML anchors, NCX boundaries, text characters, and resource limits without writing files. It retains the approved parts as private owned data and, for reconstruction, the exact decompressed XHTML bytes read from the authorized snapshot.
- `application::reconstruct` consumes a complete validated translation, emits translated character data in physical segment order, copies every protected token and every byte outside segments from the extracted originals, enforces a per-document output budget before allocation, reparses each result, and compares IDs, lexical tags and attributes, parent relationships, and NCX targets. It returns reconstructed XHTML in memory only.
- `application::batching` partitions source segments by `control.segments[].section_id` using sequential section-order packing, computes each segment's exact pretty-JSON byte cost once, verifies linear coverage and identity, and serializes each final batch once.
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
             → independent lexical coverage/integrity audit
             → complete JSON buffers [→ section-scoped batch planning/verification]
             → sibling staging directory → new output directory
```

Optional batching plans at most one section per file. It preserves source order and segment identity, computes exact serialized costs including the envelope, indentation, separators, and final newline, and never splits a segment. Batches are verified before staging and serialized once; the monolithic bytes and the independent audit are unchanged.

Validation is read-only and starts from the same bounded snapshot and extraction path:

```text
operator EPUB + supplied control + response envelopes
  → one bounded private snapshot
  → regenerate extraction and lexical validation facts
  → compare supplied control with regenerated control as a strict JSON tree
  → select the full control set or the verified expected-batch IDs
  → strict UTF-8/no-BOM JSON parse, exact schema/identity, cross-file coverage
  → marker multiplicity/topology, global atom order, XHTML anchors, NCX boundaries
  → XML character and protected-space checks
  → success summary (complete extraction or selected batch)
```

The control is never trusted for offsets or identities; the regenerated map is the only authority. A supplied expected batch selects IDs only after every source segment it contains is compared with the regenerated extraction. Translation files only supply editable text and existing marker IDs. All files are parsed and checked before the CLI prints a successful summary; the command writes no files and never produces an EPUB.

Internal reconstruction continues from a complete validation result, not from a partial batch:

```text
complete validated parts + exact original XHTML from the authorized extraction
  → segments sorted by document and physical byte range, with checked bounds
  → validated parts emitted in order; text escaped; tags/atoms/preserved ranges copied
  → per-document 32 MiB budget checked before output allocation
  → reparse → IDs, tag/attribute multiset, parent edges, NCX fragments
  → all documents in memory, or an error with no partial result
```

The validated representation owns its parts, and the extraction owns the exact XHTML bytes obtained from the same snapshot used by extraction and audit. Reconstruction does not reread the EPUB, control, or response files. Inline sibling order may differ only where validation already authorized it; protected structure and ordering checks remain authoritative. M2.1 stops at in-memory XHTML; it does not package an EPUB or expose a CLI command.

The snapshot is an automatically removed temporary file, not an extracted resource tree or persistent cache. Hashing and parsing use the same copied bytes, even if the original changes afterward. Observable source size/modification-time changes during copying cause failure. The serialized files contain no temporary paths or timestamps.

XHTML tokenization operates on the original UTF-8 buffer. Only authorized `&nbsp;` references in lexical text/attribute contexts are changed to the equal-length `&#160;` in a validation copy. Comments and CDATA are untouched by that substitution. Text decoding handles numeric references and XML line endings separately, retaining original spans, including exact protected-space locations. No full-document serialization is used to recover offsets.

Publishing uses a private temporary directory under the existing output parent. All complete JSON files or batches are written there before one native no-replace rename exposes the result. Windows uses [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) without replacement/copy flags; Linux/Android use [renameat_with](https://docs.rs/rustix/latest/rustix/fs/fn.renameat_with.html) with `NOREPLACE`. Existing files, directories, and symlinks are not replaced, including a destination appearing after preflight. Concurrent publishers cannot combine artifacts. Failed staging/publication drops only the operation's temporary resources. Abrupt termination may leave staging data; crash durability is outside this milestone.

## Data Contracts

- `InspectionReport` contains the EPUB version, package path, manifest resources, spine order, and navigation entries.
- `ManifestResource` identifies a resource by manifest ID, normalized internal path, and media type.
- `SpineEntry` preserves each `idref` and its `linear` value in declared reading order.
- `NavigationEntry` stores an optional NCX ID, title, document path, optional fragment, preorder position, and nesting depth.
- `MinamiError` distinguishes I/O, ZIP, resource-limit, XML, missing-structure, invalid-reference, ambiguity, and unsupported-format failures.

Manifest and spine order are preserved from the OPF. Navigation order follows document order in `navMap`, independent of `playOrder`. A document may have multiple navigation entries; a missing fragment remains absent.

`extract_epub(path: &Path, output: &Path) -> Result<(), MinamiError>` publishes `control.json` and `source.json` according to the normative [X format contract](x-format.md). `extract_epub_batched(path, output, batch_max_bytes)` keeps the same `control.json` and publishes a `batches/` directory of complete X envelopes capped by serialized bytes; it does not introduce a second format or publication mechanism.

`validate_epub(epub, control, translations, expected_batch) -> Result<ValidationSummary, MinamiError>` is read-only. It regenerates the extraction from a new bounded snapshot of `epub`, requires the supplied control to equal the regenerated control as a strict JSON tree, and validates one or more response envelopes against the regenerated source. Without a batch file, every extracted segment must appear exactly once across all responses. With `--expected-batch`, exactly one response is accepted and its segment set must equal the verified IDs in that batch file. `ValidationSummary { validated_segments, complete }` reports the checked count and whether the full extraction was covered; a batch run always reports `complete: false`. X records, the validated parts, the extraction, and reconstructed XHTML remain private. Internal reconstruction requires `complete == true` and returns one `{ document_id, path, source }` record per XHTML document; EPUB packaging remains future work.

## Supported EPUB Profile

Both operations support EPUB 2.0 packages with an OPF and NCX 2005-1 navigation. Reading order comes from the spine; primary navigation comes from `navMap`. Inspection preserves fragment references without resolving XHTML IDs. Extraction additionally resolves section targets and rejects missing, empty, opaque-descendant, or mid-flow targets that cannot form permitted boundaries.

Extraction covers each manifest XHTML once: unique spine documents first (including `linear="no"`), then remaining manifest XHTML. NCX aliases share normalized section boundaries; unindexed prefixes and documents remain represented. Flow groups keep mixed paragraphs together and separate nested blocks/cells. Tags, attributes, SVG, callouts, and special spaces use original spans rather than editable XHTML. The UTF-8 vocabulary and offline entity subset are defined by X 1.0; attributes and head content are not translated.

The parser rejects duplicate or unsafe paths, references that escape the EPUB root, encrypted or unsupported ZIP entries, and unsupported structural formats. ZIP and XML reads have explicit size and count limits. XML parsing does not resolve external entities or access the network. Resource bytes are checked as they are read; auxiliary manifest assets are checked for existence but are not fully inspected.

Beyond the existing ZIP limits, extraction bounds each XHTML to 32 MiB, 100,000 nodes/text chunks/attributes, 256 attributes per element, and 128 XML nesting levels. Paired inline nesting is limited to 64 levels. The extraction map permits 200,000 document/section/segment/marker/part/preserved records and 64 MiB of editable text; each serialized JSON artifact, including each batch, is capped at 64 MiB. Limit violations fail explicitly without truncation.

Validation bounds each supplied JSON file, including the control and expected batch, to 64 MiB, all translation responses together to 256 MiB and 10,000 files, and all parsed responses to 200,000 segments, 1,000,000 parts, and 64 MiB of decoded text. JSON parsing also charges one value for every scalar, object, or array and one entry for every array element or object key before constructing it, with per-file budgets of 3,000,000 values and 3,000,000 entries. A maximum legal response needs 2,600,004 values and 2,600,003 entries: one million part objects and their strings, 200,000 segment wrappers, and the envelope. The control's 200,000-record map and expected batches stay below the same bound. JSON nesting, paired-marker nesting, and NCX ancestor chains are capped at 64. The validator uses the extraction profile's existing 200,000-record map limit. These checks fail explicitly and never accept truncated input.

Reconstruction keeps the extraction profile's 32 MiB bound for each reconstructed XHTML document. Escaped replacement lengths and total output length are computed with checked arithmetic before the buffer is reserved; a document over the limit fails without allocating an oversized result. All documents must reconstruct and reparse before any result list is returned. No new input limit, public format, serializer, or dependency is introduced.

## Limitations

- EPUB 3 is not supported.
- XHTML extraction follows the explicit X vocabulary; unknown content is rejected. Computed CSS, layout, OCR, and linguistic meaning are not interpreted.
- NCX `pageList` and `navList` are not included in the report.
- The source mapping receives an independent coverage/integrity audit before publication. Translated responses are checked for structural and protected-content conformance, not translation quality, meaning, or source hyperlink/CSS equivalence. Textual attributes, head/OPF metadata, and NCX labels are outside that check.
- M2.1 reconstructs XHTML in memory only. EPUB packaging, `minami build`, non-XHTML resource reconstruction, OPF/NCX/CSS/image rewriting, and LLM integration are not implemented.
