# pdfmd

[![CI](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml/badge.svg)](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/badge/coverage-99.75%25-brightgreen)](#testing--coverage)
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
                                    # save embedded JPEGs into ./figs
                                    # and link them inline in out.md
cat input.pdf | pdfmd -             # read from stdin
pdfmd https://example.com/x.pdf     # fetched via `curl` on PATH
```

Image extraction currently passes through JPEG (`DCTDecode`) and JPEG 2000
(`JPXDecode`) streams verbatim; PDFs that store figures as `FlateDecode`
bitmaps or other filters will not yield image files.

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

## Testing & coverage

326 tests (311 unit + 15 integration). Run them with:

```sh
cargo test --all-targets
```

Reproduce the coverage numbers in the badge:

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --summary-only --ignore-filename-regex 'src/bin/'
```

The `src/bin/` exclusion drops `profile.rs` — a developer-only profiling
harness that isn't part of the shipped library or CLI.

Current breakdown:

| file                  | lines  | regions | functions |
|-----------------------|--------|---------|-----------|
| `extract/cmap.rs`     | 99.57% | 98.90%  | 100.00%   |
| `extract/content.rs`  | 99.10% | 98.76%  | 100.00%   |
| `extract/encoding.rs` | 100.00%| 100.00% | 100.00%   |
| `extract/font.rs`     | 99.46% | 99.43%  | 100.00%   |
| `extract/glyphs.rs`   | 100.00%| 100.00% | 100.00%   |
| `extract/image.rs`    | 100.00%| 98.77%  | 100.00%   |
| `extract/mod.rs`      | 99.13% | 98.75%  | 100.00%   |
| `extract/parser.rs`   | 99.52% | 99.55%  | 100.00%   |
| `heuristics.rs`       | 100.00%| 100.00% | 100.00%   |
| `lib.rs`              | 100.00%| 100.00% | 100.00%   |
| `main.rs`             | 99.68% | 99.35%  | 100.00%   |
| `pdf/deflate.rs`      | 99.75% | 99.24%  | 100.00%   |
| `pdf/mod.rs`          | 99.88% | 99.77%  | 100.00%   |
| `pdf/object.rs`       | 100.00%| 100.00% | 100.00%   |
| `pdf/parser.rs`       | 99.68% | 99.34%  | 98.67%    |
| **total**             | **99.75%** | **99.52%** | **99.83%** |

The remaining 0.25% is split between closing-brace regions of `if let`
arms whose unmatched pattern is never observed by a passing test, `?`
error arms in DEFLATE where the only way to fail is a hand-crafted bit
stream that errors mid-block, and a couple of OS-level I/O failure paths
(stdin/stdout writes) that would need a subprocess to trigger.

## How it works

```
src/
├── pdf/
│   ├── deflate.rs    zlib + DEFLATE decoder (RFC 1950 / 1951)
│   ├── object.rs     Object / Dictionary / Stream model
│   ├── parser.rs     byte-level object parser
│   └── mod.rs        Document, xref, object streams, /FlateDecode chain
├── extract/
│   ├── encoding.rs   WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs     glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs       ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs       per-font byte → text decoder
│   ├── parser.rs     streaming content-stream tokenizer
│   ├── content.rs    content-stream interpreter and text-state machine
│   └── mod.rs        page walking + per-page parallelism
├── heuristics.rs     headings, lists, paragraph reflow
├── lib.rs            public API
└── main.rs           CLI
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
