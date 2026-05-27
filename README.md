# pdfmd

[![CI](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml/badge.svg)](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/badge/coverage-100%25-brightgreen)](#testing--coverage)
[![Dependencies](https://img.shields.io/badge/dependencies-0-brightgreen)](Cargo.toml)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-blue)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

A fast, **zero-dependency** PDF → Markdown converter written in Rust.

`pdfmd` extracts text directly from a PDF — parsing the object graph,
inflating compressed streams, decoding fonts, and interpreting the
content-stream operators itself — then runs a small set of heuristics to
recover headings, lists, and paragraph boundaries. Every layer, including
the zlib/DEFLATE decoder and the PDF reader, is implemented in this crate.

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
`DeviceCMYK` raster image XObjects into PNG.

## Performance

End-to-end CLI benchmark on a 1.05 MB, 17-page arXiv paper (Apple Silicon,
release build, `hyperfine --warmup 5 --runs 20 -N`):

| metric            | value                |
|-------------------|----------------------|
| min               | 3.8 ms               |
| mean              | 4.4 ms ± 0.3 ms      |
| throughput        | ~240 MB/s of PDF     |
| pages/sec         | ~3,900               |
| release binary    | ~600 KB              |

Per-page font and content-stream work runs across a small
`std::thread::scope` worker pool, and fonts shared across pages are parsed
once into a document-wide cache. The content-stream tokenizer and the
DEFLATE decoder both borrow operands directly from the source bytes, so
the hot path doesn't allocate per operator or per Huffman code.

The [LiteParse v2 announcement][liteparse-v2] reports timings for four
PDF shapes. Mirroring those page counts with local synthetic PDFs, warmed
in-process `pdfmd` conversion measured:

| document shape | LiteParse v2 published | `pdfmd` local run | speedup | less wall time |
|----------------|------------------------|-------------------|---------|----------------|
| `1_page.pdf` | `2.0 ms` | `0.012 ms` | ~167x | ~99.4% |
| `24_pages.pdf` | `41.0 ms` | `0.118 ms` | ~347x | ~99.7% |
| `60_pages.pdf` | `123.0 ms` | `0.201 ms` | ~612x | ~99.8% |
| `457_pages.pdf` | `777.0 ms` | `0.933 ms` | ~833x | ~99.9% |

Those comparisons are throughput targets against the published LiteParse
v2 numbers, not a claim against LiteParse's exact benchmark corpus or
hardware. The 457-page local input is a synthetic 100 MB PDF; the smaller
rows are synthetic text PDFs with matching page counts.

With image extraction enabled on the 457-page, 100 MB local input,
`target/release/pdfmd ... --extract-images` averaged `52.4 ms ± 10.0 ms`
over 10 `hyperfine` runs. That is still ~14.8x faster than the published
`0.777 s` target. A decoded-raster image fixture, which exercises PNG
generation instead of JPEG pass-through, averaged `170.2 ms ± 4.8 ms`.

[liteparse-v2]: https://www.llamaindex.ai/blog/liteparse-v2-0-runs-everywhere

## Testing & coverage

389 tests (362 unit + 27 integration). Run them with:

```sh
cargo test --all-targets
```

Reproduce the coverage numbers in the badge:

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --all-targets --summary-only --ignore-filename-regex 'src/bin/'
```

The `src/bin/` exclusion drops `profile.rs` — a developer-only profiling
harness that isn't part of the shipped library or CLI.

Current breakdown:

| file                  | lines  | regions | functions |
|-----------------------|--------|---------|-----------|
| `cli.rs`              | 100.00%| 100.00% | 100.00%   |
| `extract/cmap.rs`     | 100.00%| 99.49%  | 100.00%   |
| `extract/content.rs`  | 100.00%| 99.38%  | 100.00%   |
| `extract/encoding.rs` | 100.00%| 100.00% | 100.00%   |
| `extract/font.rs`     | 100.00%| 100.00% | 100.00%   |
| `extract/glyphs.rs`   | 100.00%| 100.00% | 100.00%   |
| `extract/image.rs`    | 100.00%| 98.22%  | 100.00%   |
| `extract/image/png.rs`| 100.00%| 99.65%  | 100.00%   |
| `extract/mod.rs`      | 100.00%| 100.00% | 100.00%   |
| `extract/parser.rs`   | 100.00%| 99.85%  | 100.00%   |
| `heuristics.rs`       | 100.00%| 100.00% | 100.00%   |
| `lib.rs`              | 100.00%| 100.00% | 100.00%   |
| `main.rs`             | 100.00%| 100.00% | 100.00%   |
| `pdf/deflate.rs`      | 100.00%| 100.00% | 100.00%   |
| `pdf/filter.rs`       | 100.00%| 99.69%  | 100.00%   |
| `pdf/mod.rs`          | 100.00%| 99.90%  | 100.00%   |
| `pdf/object.rs`       | 100.00%| 100.00% | 100.00%   |
| `pdf/object_stream.rs`| 100.00%| 98.88%  | 100.00%   |
| `pdf/page_tree.rs`    | 100.00%| 97.83%  | 100.00%   |
| `pdf/parser.rs`       | 100.00%| 100.00% | 100.00%   |
| `pdf/xref.rs`         | 100.00%| 100.00% | 100.00%   |
| **total**             | **100.00%** | **99.77%** | **100.00%** |

The badge tracks line coverage. The table also includes llvm-cov region
coverage, which is stricter about expression-level instrumentation.

## How it works

```
src/
├── pdf/
│   ├── deflate.rs    zlib + DEFLATE decoder (RFC 1950 / 1951)
│   ├── filter.rs     stream filters and PNG predictor decoding
│   ├── object.rs     Object / Dictionary / Stream model
│   ├── object_stream.rs  PDF 1.5 object stream unpacking
│   ├── page_tree.rs  catalog / page tree traversal
│   ├── parser.rs     byte-level object parser
│   ├── xref.rs       classic xref tables and xref streams
│   └── mod.rs        Document facade and object cache
├── extract/
│   ├── encoding.rs   WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs     glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs       ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs       per-font byte → text decoder
│   ├── image.rs      image XObject detection and Markdown asset wiring
│   ├── image/
│   │   └── png.rs    minimal PNG encoder for decoded image rasters
│   ├── parser.rs     streaming content-stream tokenizer
│   ├── content.rs    content-stream interpreter and text-state machine
│   └── mod.rs        page walking + per-page parallelism
├── heuristics.rs     headings, lists, paragraph reflow
├── lib.rs            public API
├── cli.rs            CLI parser and execution
└── main.rs           process entrypoint
```

## Limitations

- Tables, multi-column figure captions, and complex math layout come out
  as best-effort reflowed text.
- Fonts that ship without a `ToUnicode` CMap and use neither a standard
  encoding nor a `/Differences` array will silently drop glyphs.
- The heuristic layer targets academic and prose documents. Forms,
  invoices, and other heavily-structured PDFs will not reconstruct well.
- Encrypted PDFs and `LZWDecode` streams are not supported.

## License

MIT.
