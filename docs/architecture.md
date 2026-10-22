# Architecture

## Technology Stack

Minami uses Rust 2024 and Cargo. `zip` reads the EPUB container, `roxmltree` parses XML, `serde` and `serde_json` define and serialize reports, `clap` handles the CLI, and `thiserror` defines typed errors.

## Architectural Style

The implementation follows Data-Oriented Design: explicit data structures, small focused functions, and a clear flow of data, without object-oriented hierarchies. The CLI, application, and EPUB parser have separate responsibilities. Ordered results use vectors, parser errors are propagated explicitly, and invalid or ambiguous input fails with an error. Given the same EPUB, the report order is deterministic. Inspection is read-only and does not repair EPUB files.

## Modules

```text
main
└── cli
    └── application::inspect
        └── epub
            ├── archive
            ├── package
            └── navigation

model and error provide shared contracts
```

- `main` starts the command-line application.
- `cli` parses `inspect` arguments and presents a completed report as JSON; it does not parse XML.
- `application::inspect` coordinates the EPUB readers and assembles the report.
- `epub::archive` validates and reads ZIP entries without extracting them to disk.
- `epub::package` reads `container.xml`, locates the OPF, and parses its manifest and spine.
- `epub::navigation` parses the NCX `navMap` and validates its document targets.
- `model` defines the public report structures; `error` defines errors shared by the library and CLI.

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

## Data Contracts

- `InspectionReport` contains the EPUB version, package path, manifest resources, spine order, and navigation entries.
- `ManifestResource` identifies a resource by manifest ID, normalized internal path, and media type.
- `SpineEntry` preserves each `idref` and its `linear` value in declared reading order.
- `NavigationEntry` stores an optional NCX ID, title, document path, optional fragment, preorder position, and nesting depth.
- `MinamiError` distinguishes I/O, ZIP, resource-limit, XML, missing-structure, invalid-reference, ambiguity, and unsupported-format failures.

Manifest and spine order are preserved from the OPF. Navigation order follows document order in `navMap`, independent of `playOrder`. A document may have multiple navigation entries; a missing fragment remains absent.

## Supported EPUB Profile

The current profile supports EPUB 2.0 packages with an OPF, an NCX 2005-1 navigation document, and XHTML reading resources. The reading order comes from the OPF spine; the primary navigation comes from NCX `navMap`. Multiple logical sections may point to the same XHTML document. Fragment references are preserved but are not checked against IDs inside XHTML.

The parser rejects duplicate or unsafe paths, references that escape the EPUB root, encrypted or unsupported ZIP entries, and unsupported structural formats. ZIP and XML reads have explicit size and count limits. XML parsing does not resolve external entities or access the network. Resource bytes are checked as they are read; auxiliary manifest assets are checked for existence but are not fully inspected.

## Limitations

- EPUB 3 is not supported.
- XHTML is checked as a required resource but is not interpreted semantically.
- NCX `pageList` and `navList` are not included in the report.
- Text extraction and fragment-ID validation are not implemented.
- EPUB reconstruction and LLM integration are not implemented.
