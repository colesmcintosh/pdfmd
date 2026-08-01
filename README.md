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

### Corpus benchmark

A broader end-to-end CLI benchmark covers 10 public PDFs: 1,120 pages and
19.6 MB spanning papers, standards, forms, a handbook, and a typeset legal
document. Both binaries include the simple-font correctness fix; the optimized
variant additionally uses compact DEFLATE Huffman tables and process-wide
caching for the fixed RFC 1951 tables. Their Markdown output is byte-identical.

Each result is the arithmetic mean of 20 measured release-build executions
after five warmups. Document and binary order were shuffled within paired
blocks, and output was written to `/dev/null`.

| document | pages | baseline | optimized | speedup |
|----------|------:|---------:|----------:|--------:|
| [Attention Is All You Need](https://arxiv.org/pdf/1706.03762) | 15 | 11.48 ms | 10.96 ms | 1.05x |
| [BERT](https://arxiv.org/pdf/1810.04805) | 16 | 4.58 ms | 4.28 ms | 1.07x |
| [Bitcoin whitepaper](https://bitcoin.org/bitcoin.pdf) | 9 | 3.47 ms | 3.40 ms | 1.02x |
| [IRS Form W-9](https://www.irs.gov/pub/irs-pdf/fw9.pdf) | 6 | 8.03 ms | 4.16 ms | 1.93x |
| [NASA Systems Engineering Handbook](https://www.nasa.gov/wp-content/uploads/2018/09/nasa_systems_engineering_handbook_0.pdf) | 297 | 168.68 ms | 91.01 ms | 1.85x |
| [NIST Cybersecurity Framework 2.0](https://nvlpubs.nist.gov/nistpubs/CSWP/NIST.CSWP.29.pdf) | 32 | 13.85 ms | 6.03 ms | 2.30x |
| [NIST FIPS 180-4](https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.180-4.pdf) | 36 | 5.50 ms | 4.63 ms | 1.19x |
| [RFC 9110: HTTP Semantics](https://www.rfc-editor.org/rfc/rfc9110.pdf) | 194 | 26.33 ms | 23.91 ms | 1.10x |
| [Constitution of the United States](https://www.govinfo.gov/content/pkg/CDOC-110hdoc50/pdf/CDOC-110hdoc50.pdf) | 85 | 7.67 ms | 6.63 ms | 1.16x |
| [W3C CSS 2.1](https://www.w3.org/TR/2011/REC-CSS2-20110607/css2.pdf) | 430 | 229.52 ms | 240.94 ms | 0.95x |
| **sum of document means** | **1,120** | **479.11 ms** | **395.96 ms** | **1.21x** |

Nine documents improve, with a maximum 2.30x speedup. CSS 2.1 is about 5%
slower and remains a useful stress case for the DEFLATE symbol decoder.

### Extraction validation

All 10 corpus documents convert successfully. Comparing case-folded Unicode
word tokens against independent Poppler `pdftotext` output gives 86.6–99.8%
recall and 92.8–99.8% precision, with no Unicode replacement characters or
unexpected control characters. These numbers measure text fidelity, not
semantic Markdown reconstruction: multi-column reading order, tables, code
blocks, and heading inference remain best-effort.

## Testing & coverage

341 tests (326 unit + 15 integration). Run them with:

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
