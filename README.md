# pdfmd

[![CI](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml/badge.svg)](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/badge/coverage-99%25-brightgreen)](#testing--coverage)
[![Dependencies](https://img.shields.io/badge/dependencies-0-brightgreen)](Cargo.toml)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-blue)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Site](https://img.shields.io/badge/site-github.io-blue)](https://colesmcintosh.github.io/pdfmd/)

A fast, **zero-dependency** PDF → Markdown converter written in Rust.

Site: [colesmcintosh.github.io/pdfmd](https://colesmcintosh.github.io/pdfmd/).

`pdfmd` extracts text directly from a PDF — parsing the object graph,
inflating compressed streams, decoding fonts, and interpreting the
content-stream operators itself — then formats positioned spans into
Markdown. The heuristic layer recovers columns, GFM tables, headings,
lists, emphasis, and paragraph boundaries. Every layer, including the
zlib/DEFLATE decoder and the PDF reader, is implemented in this crate.

## Why

PDFs do not carry semantic structure. Most extraction libraries either
return one undifferentiated text blob, or stop at the slow, general-purpose
parsing layer. `pdfmd` skips both: it owns the path from bytes to
Markdown, which keeps the conversion around ~4 ms for a typical academic
paper and leaves room to tune the heuristics for the documents you care
about.

`[dependencies]` is empty. The CLI parser, error type, work scheduler,
PDF reader, and DEFLATE decoder all live in this crate.

## Install

```sh
cargo install --path .
```

Or build a release binary directly:

```sh
cargo build --release
# binary is at ./target/release/pdfmd
```

## Usage

```sh
pdfmd input.pdf                     # markdown to stdout
pdfmd input.pdf -o output.md        # write to a file
pdfmd input.pdf --page-breaks       # insert `---` between PDF pages
pdfmd input.pdf --extract-images figs -o out.md
                                    # save supported embedded images into ./figs
                                    # and link them inline in out.md
cat input.pdf | pdfmd -             # read from stdin
pdfmd https://example.com/x.pdf     # fetched via `curl` on PATH
```

Image extraction passes through JPEG (`DCTDecode`) and JPEG 2000
(`JPXDecode`) streams verbatim, including simple ASCIIHex/ASCII85 wrapper
chains. It also converts decoded 8-bit `DeviceGray`, `DeviceRGB`, and
`DeviceCMYK` raster image XObjects into PNG. Resource discovery follows
nested Form XObjects and deduplicates shared images by object ID.

## Library

The same converter is a Rust API with one error type:

```rust
use pdfmd::{convert_pdf_to_markdown, ConvertOptions};

let result = convert_pdf_to_markdown(&pdf_bytes, &ConvertOptions::default())?;
print!("{}", result.markdown);
```

## Markdown reconstruction

Conversion walks positioned spans (not a flat text dump) and:

- Reads multi-column pages left-to-right, then top-to-bottom in each column.
- Emits GFM tables from ruled path grids and from aligned borderless columns.
- Infers headings from tagged-PDF roles, font size, bold, numbered sections,
  and names such as `Abstract` / `Introduction`.
- Keeps bullet and ordered lists, bold/italic from the font name, and
  monospace runs as fenced code blocks.
- Strips repeating running headers and footers, and joins hyphenated
  line breaks.

The first paragraph is promoted to `#` in `promote_document_title`.

## Performance

End-to-end CLI benchmark on a 1.05 MB, 17-page arXiv paper (Apple Silicon,
release build, `hyperfine --warmup 5 --runs 20 -N`):

| metric            | value                |
|-------------------|----------------------|
| min               | 3.8 ms               |
| mean              | 4.4 ms ± 0.3 ms      |
| throughput        | ~240 MB/s of PDF     |
| pages/sec         | ~3,900               |
| release binary    | ~700 KB              |

Per-page font and content-stream work runs across a small
`std::thread::scope` worker pool, and fonts shared across pages are parsed
once into a document-wide cache. Multi-page Markdown formatting uses the
same pool. The content-stream tokenizer and the DEFLATE decoder both borrow
operands directly from the source bytes, so the hot path doesn't allocate
per operator or per Huffman code.

### Published comparison targets

Other libraries publish timings for four common PDF shapes. Mirroring those
page counts with local synthetic PDFs, warmed in-process `pdfmd` conversion
measured:

| document shape | other-library published | `pdfmd` local run | speedup | less wall time |
|----------------|-------------------------|-------------------|---------|----------------|
| `1_page.pdf` | `2.0 ms` | `0.012 ms` | ~167x | ~99.4% |
| `24_pages.pdf` | `41.0 ms` | `0.118 ms` | ~347x | ~99.7% |
| `60_pages.pdf` | `123.0 ms` | `0.201 ms` | ~612x | ~99.8% |
| `457_pages.pdf` | `777.0 ms` | `0.933 ms` | ~833x | ~99.9% |

Those comparisons are throughput targets against other published library
numbers, not a claim against those exact benchmark corpora or hardware.
The 457-page local input is a synthetic 100 MB PDF; the smaller rows are
synthetic text PDFs with matching page counts.

With image extraction enabled on the 457-page, 100 MB local input,
`target/release/pdfmd ... --extract-images` averaged `52.4 ms ± 10.0 ms`
over 10 `hyperfine` runs. That is still ~14.8x faster than the published
`0.777 s` target. A decoded-raster image fixture, which exercises PNG
generation instead of JPEG pass-through, averaged `170.2 ms ± 4.8 ms`.

### Public corpus benchmark

A broader end-to-end CLI benchmark covers 10 public PDFs: 1,120 pages and
19.6 MB spanning papers, standards, forms, a handbook, and a typeset legal
document. A warm-cache batch benchmark converts all 10 documents to
`/dev/null` in each measured command (`hyperfine --warmup 3 --runs 20`):

| build | mean | relative throughput |
|-------|-----:|--------------------:|
| before this optimization pass | 390.6 ms ± 42.4 ms | 1.00x |
| current | 158.8 ms ± 12.1 ms | 2.46x |

The largest individual gain is CSS 2.1, which falls from 237.8 ms to
22.6 ms (10.51x); the object-stream-heavy NASA handbook falls from 84.6 ms
to 72.4 ms (1.17x). Recursive Form XObject interpretation intentionally does
more work on papers that place diagrams or equations in Forms: those small
documents can add roughly 1–2 ms while recovering text the old extractor never
visited.

### Extraction validation

All 10 corpus documents convert successfully. Comparing 431,422 case-folded
Unicode word tokens against independent Poppler `pdftotext` output gives
98.02% weighted recall and 97.16% weighted precision. Recall rises from
96.79%; the small precision tradeoff from 97.31% reflects additional Form
text. Token F1 rises from 97.05% to 97.59%, with 5,317 additional reference
tokens recovered and no Unicode replacement characters or unexpected control
characters. Per-document recall is 90.8–99.9% and precision is 91.1–99.8%.

These numbers measure text fidelity, not semantic Markdown reconstruction:
column order, tables, code blocks, and heading inference are layout
heuristics and remain best-effort on complex pages.

## Testing & coverage

457 tests (430 unit + 27 integration). Run them with:

```sh
cargo test --all-targets
```

Reproduce the coverage numbers in the badge:

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --all-targets --summary-only --ignore-filename-regex 'src/bin/'
```

The `src/bin/` exclusion drops `profile.rs` and `opendataloader.rs` —
developer-only binaries that aren't part of the shipped library or CLI.

Current breakdown:

| file                   | lines   | regions | functions |
|------------------------|---------|---------|-----------|
| `cli.rs`               | 100.00% | 100.00% | 100.00%   |
| `extract/cmap.rs`      | 100.00% | 98.64%  | 100.00%   |
| `extract/content.rs`   | 97.44%  | 92.43%  | 97.85%    |
| `extract/encoding.rs`  | 99.81%  | 99.80%  | 100.00%   |
| `extract/font.rs`      | 100.00% | 100.00% | 100.00%   |
| `extract/glyphs.rs`    | 99.85%  | 99.85%  | 100.00%   |
| `extract/image.rs`     | 100.00% | 94.18%  | 100.00%   |
| `extract/image/png.rs` | 100.00% | 98.51%  | 100.00%   |
| `extract/layout.rs`    | 100.00% | 100.00% | 100.00%   |
| `extract/mod.rs`       | 99.57%  | 97.60%  | 100.00%   |
| `extract/parser.rs`    | 99.58%  | 98.60%  | 100.00%   |
| `extract/structure.rs` | 95.91%  | 91.84%  | 100.00%   |
| `heuristics/mod.rs`    | 95.80%  | 90.76%  | 97.20%    |
| `heuristics/tables.rs` | 97.45%  | 93.01%  | 96.15%    |
| `lib.rs`               | 100.00% | 100.00% | 100.00%   |
| `main.rs`              | 100.00% | 100.00% | 100.00%   |
| `pdf/deflate.rs`       | 100.00% | 100.00% | 100.00%   |
| `pdf/filter.rs`        | 100.00% | 99.19%  | 100.00%   |
| `pdf/mod.rs`           | 99.68%  | 97.71%  | 98.43%    |
| `pdf/object.rs`        | 100.00% | 100.00% | 100.00%   |
| `pdf/object_stream.rs` | 100.00% | 95.65%  | 100.00%   |
| `pdf/page_tree.rs`     | 100.00% | 95.56%  | 100.00%   |
| `pdf/parser.rs`        | 99.87%  | 99.56%  | 100.00%   |
| `pdf/syntax.rs`        | 100.00% | 95.24%  | 100.00%   |
| `pdf/test_pdf.rs`      | 100.00% | 100.00% | 100.00%   |
| `pdf/xref.rs`          | 100.00% | 100.00% | 100.00%   |
| `util.rs`              | 98.65%  | 97.22%  | 91.67%    |
| **total**              | **99.04%** | **97.17%** | **99.06%** |

The badge tracks line coverage. The table also includes llvm-cov region
coverage, which is stricter about expression-level instrumentation.

## How it works

```
src/
├── pdf/
│   ├── deflate.rs        zlib + DEFLATE decoder (RFC 1950 / 1951)
│   ├── filter.rs         stream filters and PNG predictor decoding
│   ├── object.rs         Object / Dictionary / Stream model
│   ├── object_stream.rs  PDF 1.5 object stream unpacking
│   ├── page_tree.rs      catalog / page tree traversal
│   ├── parser.rs         byte-level object parser
│   ├── syntax.rs         shared ISO 32000 whitespace / hex helpers
│   ├── xref.rs           classic xref tables and xref streams
│   └── mod.rs            Document facade and object cache
├── extract/
│   ├── encoding.rs       WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs         glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs           ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs           per-font byte → text decoder
│   ├── image.rs          image XObject detection and Markdown asset wiring
│   ├── image/
│   │   └── png.rs        minimal PNG encoder for decoded image rasters
│   ├── parser.rs         streaming content-stream tokenizer
│   ├── content.rs        content/Form interpreter and text-state machine
│   ├── layout.rs         positioned spans, font-style hints, path rects
│   ├── structure.rs      tagged-PDF structure tree → heading/table/list roles
│   └── mod.rs            page walking + per-page parallelism
├── heuristics/
│   ├── tables.rs         ruled and borderless GFM tables
│   └── mod.rs            columns, headings, lists, emphasis, header/footer strip
├── util.rs               shared parallel_map worker pool
├── lib.rs                public API
├── cli.rs                CLI parser and execution
└── main.rs               process entrypoint
```

## Limitations

- Ruled and borderless tables are recovered when the grid is clear.
  Spanned cells, nested tables, and complex math layout remain
  best-effort reflowed text.
- Two-column body text is read left-then-right; captions that straddle
  columns can still come out jumbled.
- Fonts that ship without a `ToUnicode` CMap and use neither a standard
  encoding nor a `/Differences` array will silently drop glyphs.
- The heuristic layer targets academic and prose documents. Forms,
  invoices, and other heavily-structured PDFs will not reconstruct well.
- Encrypted PDFs and `LZWDecode` streams are not supported.

## License

MIT.
