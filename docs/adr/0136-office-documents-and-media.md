# ADR-0136: Office documents, OCR confidence, and PDF metadata

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Agent Tools, Media
- **Related:** ADR-0134, ADR-0135, ADR-0125

## Context

ADR-0135 shipped bounded PDF text, image metadata/OCR, and structured data
readers, but left office documents unimplemented and reported OCR only as raw
text. Agents working with specifications, slide decks, and spreadsheets still
had to resort to shell heuristics or could not read the file at all.

Office containers are ZIP archives with XML parts. They are untrusted input:
archive metadata can lie about expansion sizes, member names can attempt path
traversal, XML can contain DTDs/entities, and a valid workbook can contain
millions of cells or repeated spreadsheet cells. A production reader must bound
all of those before any XML reaches the model.

## Decision

### 1. `read_document` is a deterministic, archive-safe reader

The new read-only tool supports:

- DOCX (WordprocessingML) and ODT: headings, paragraphs, and tables in document
  order;
- PPTX (PresentationML) and ODP: slide-by-slide text in presentation order;
- XLSX (SpreadsheetML) and ODS: sheet names, shared strings/text values, booleans,
  errors, and bounded rows/columns.

The reader never extracts archive members to disk. It opens the file directly,
validates the central directory, and reads only the XML parts needed for the
requested format into bounded memory buffers.

### 2. ZIP safety limits are explicit and enforced before parsing

- container file: 128 MiB;
- one member's declared uncompressed size: 64 MiB;
- total declared uncompressed size: 256 MiB;
- member count: 10,000;
- duplicate member names, encrypted members, NUL/backslash names, absolute
  paths, `..` components, and names that do not enclose under the archive root
  are rejected;
- actual bytes read are capped again with `Read::take`, so a false declared size
  cannot force an unbounded allocation;
- only documented XML parts are opened; embedded images, macros, OLE objects,
  and external relationships are never executed or followed.

### 3. XML is parsed as untrusted input

`quick-xml` handles tokenization. The parser rejects DTD declarations, caps XML
nesting at 512 elements, resolves only predefined XML entities and numeric
character references, and renders unknown entities literally with a warning.
It never performs external entity resolution. Part contents may be UTF-8 or
BOM-marked UTF-16; other encodings are rejected explicitly. Output is capped at
200,000 characters, with per-block/cell/per-format caps below that.

### 4. Filters and structured results are deterministic

- `pages` is a 1-based slide selection for PPTX/ODP. It is ignored for formats
  without page numbers rather than pretending to apply.
- `sheets` is a case-insensitive, comma-separated (or string-array) sheet
  selection for XLSX/ODS. Unknown names produce warnings; a selection with no
  matches is an explicit `SHEET_NOT_FOUND` error listing the available sheets.
- `include_tables` controls DOCX/ODT table rendering; omitted tables are named in
  the text but their rows are not returned in structured data.
- `max_rows`/`max_cols` bound spreadsheet extraction per sheet. Hard runtime
  caps also apply regardless of requested values.
- Structured output includes `format`, `entries`, `archive_entries`, `chars`,
  `extracted_chars`, `truncated`, `warnings`, plus `tables` or `slides` or
  `sheets`/`sheet_names` as appropriate.

### 5. Honest office-format limitations

- DOCX/ODT text extraction preserves paragraph, heading, and table order; it
  does not reconstruct page layout, columns, footnotes, comments, or floating
  text boxes.
- PPTX/ODP extraction reads slide text, not speaker notes, animations, images, or
  chart data.
- XLSX/ODS extraction returns displayed/shared text and raw numeric values. It
  does not apply Excel/Calc number formats, date serial conversion, formulas, or
  cell styles. Formula cells without cached values are reported as empty with a
  warning.
- Macros, embedded objects, and encrypted documents are not supported.
- Document conversion is intentionally not delegated to LibreOffice, pandoc, or
  another host binary; a parser failure is reported, not hidden behind a shell
  invocation.

### 6. `read_image` reports OCR confidence when tesseract provides it

OCR still runs only when requested and only when `tesseract` is installed. The
tool now invokes the TSV output mode and derives:

- OCR text with line grouping;
- word count;
- confidence mean (0–100), minimum, and maximum, weighted by word length.

Missing tesseract remains a warning, not a fabricated result. Tesseract output
that cannot be parsed as TSV is reported as a warning, and images without OCR
return metadata only.

### 7. `read_pdf` uses PDF metadata when available

The reader still extracts text with the existing in-process extractor and keeps
best-effort form-feed page boundaries. It now also loads the PDF with `lopdf`
(on a blocking task) to obtain:

- page count, exposed as `metadata_page_count` and used for `page_count` when
  available;
- title, author, subject, creator, producer, creation date, and modification
  date where present.

`page_count_source` reports `pdf-metadata`, `text-boundaries`, or `unknown`.
Page selection still requires real text boundaries; when they are unavailable
the full text is returned with a warning even if metadata knows the page count.
PDFDocEncoding metadata strings are approximated as Latin-1 when they are not
UTF-8/UTF-16; this is display metadata only and is never used for security
decisions.

## Consequences

Positive:

- Agents can read common office documents with one call instead of shell
  guessing or manual XML extraction.
- Archive bombs, traversal names, encrypted members, DTDs, and unbounded
  structures are rejected before document parsing.
- Spreadsheet and slide filters return bounded, structured evidence.
- OCR confidence gives the model a reason to distrust low-quality OCR.
- PDF page counts no longer depend only on extractor form feeds.

Negative / limitations:

- Office formatting fidelity is intentionally limited to readable text/tables;
  this is not a rendering or conversion engine.
- OOXML/ODF XML parsing is namespace-prefix tolerant but not a full schema
  validator; malformed parts can be skipped per slide/sheet with warnings.
- Date formatting, formulas, charts, notes, and embedded media are not
  interpreted.
- ZIP and XML caps mean very large documents require an external conversion
  workflow.
- PDF metadata decoding is best-effort for legacy encodings.

## Alternatives

- Shell out to LibreOffice/pandoc: rejected because it adds a large host
  dependency, unbounded subprocess/resource behavior, and a conversion path that
  is harder to sandbox and audit.
- Extract the archive to a temporary directory: rejected; all parsing stays
  in-memory and never creates a new filesystem write surface.
- Accept all ZIP entries and rely on the XML parser alone: rejected; member
  count/size/traversal checks must happen before decompression.
- Claim page-accurate PDF selection from metadata alone: rejected; metadata
  gives a count but not text boundaries.
- Use a model to interpret spreadsheet cells or slides: rejected; deterministic
  bounds and explicit warnings are cheaper, inspectable, and safer.
