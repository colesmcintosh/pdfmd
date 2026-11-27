# pdfmd

[![CI](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml/badge.svg)](https://github.com/colesmcintosh/pdfmd/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/badge/coverage-99%25-brightgreen)](#testing--quality)
[![Dependencies](https://img.shields.io/badge/dependencies-0-brightgreen)](Cargo.toml)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-blue)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Site](https://img.shields.io/badge/site-github.io-blue)](https://colesmcintosh.github.io/pdfmd/)

A fast, **zero-dependency** PDF → Markdown converter written in Rust.

`pdfmd` owns the entire stack from PDF bytes to Markdown — xref and object-stream reader, zlib/DEFLATE decoder, fonts and `ToUnicode` CMaps, content-stream interpreter, layout heuristics, and CLI parser — with an empty `[dependencies]` table. See [`VISION.md`](VISION.md) for the design principles and [colesmcintosh.github.io/pdfmd](https://colesmcintosh.github.io/pdfmd/) for an overview.

## Install

```sh
cargo install pdfmd          # crates.io (Rust 1.70+)
cargo install --path .       # from a checkout
```

Tagged releases also attach unsigned CLI binaries (`pdfmd` / `pdfmd.exe`) plus `SHA256SUMS` for Linux (`x86_64` / `aarch64` static musl), macOS (`x86_64` / `arm64`, 11+), and Windows (`x86_64` MSVC).

## CLI

```sh
pdfmd input.pdf                             # markdown to stdout
pdfmd input.pdf -o out.md                   # write to file
pdfmd input.pdf --page-breaks               # insert `---` between pages
pdfmd input.pdf --extract-images figs -o out.md
cat input.pdf | pdfmd -                     # read from stdin
pdfmd https://example.com/x.pdf             # fetched via `curl` on PATH
```

Image extraction passes through JPEG (`DCTDecode`) and JPEG 2000 (`JPXDecode`) verbatim, encodes decoded 8-bit Gray/RGB/CMYK rasters to PNG, follows nested Form XObjects, and deduplicates shared images by object ID.

## Library

### Rust

```rust
use pdfmd::{convert_pdf_to_markdown, ConvertOptions};

let result = convert_pdf_to_markdown(&pdf_bytes, &ConvertOptions::default())?;
print!("{}", result.markdown); // result.images is populated when image_dir is set
```

### Python

The crate builds a `cdylib` (`src/ffi.rs`) bound from `python/` via `ctypes` — no PyO3, no maturin, and no runtime packages. The PyPI name `pdfmd` belongs to an unrelated project, so install from a checkout:

```sh
pip install ./python
```

```python
import pdfmd

result = pdfmd.convert_file("paper.pdf", page_breaks=True, image_dir="figs")
print(result.markdown)
result.write_images("figs")

# Batch conversion across a thread pool (returns results in input order)
batch = pdfmd.convert_many(["a.pdf", "b.pdf", raw_bytes], workers=8)
```

An installed wheel also exposes the `pdfmd` CLI (or `python -m pdfmd` from a checkout) with the same flags as the Rust binary, using `urllib` for URL inputs. Set `PDFMD_LIBRARY` to point at an explicit shared library.

### C ABI

```c
PdfmdResult *r = pdfmd_convert(bytes, len, false, NULL);
if (r->error) fwrite(r->error, 1, r->error_len, stderr);
else          fwrite(r->markdown, 1, r->markdown_len, stdout);
pdfmd_result_free(r);
```

All buffers are `(pointer, length)` pairs with no implied NUL; free every result once with `pdfmd_result_free`.

## Markdown reconstruction

`pdfmd` walks positioned spans measured from real font glyph widths (`/Widths`, `/W`):

- **Columns & bands:** reads multi-column pages left-to-right, top-to-bottom, keeping full-width titles, captions, and tables whole in place while separating side-by-side column tables.
- **Tables:** emits GFM tables from connected ruled grids and aligned borderless/booktabs columns — handling narrow numeric columns, empty cells, centered `\multirow` labels, staggered headers, and `\rowcolor` shaded bands while filtering underlines, callouts, and justified prose.
- **Headings:** recovers hierarchy from tagged-PDF roles, font size, bold (including wrapped multi-line headings), numbered sections (`3.1`, `A.1`), and named sections (`Abstract`, `Introduction`).
- **Text & margins:** preserves bullet/ordered lists (including wrapped multi-line items), bold/italic emphasis, monospace fenced code blocks, hyphenated word breaks, repeating header/footer stripping, and rotated margin stamps.

## Performance

End-to-end CLI benchmark on a 1.05 MB, 17-page arXiv paper (Apple Silicon, release build, `hyperfine --warmup 5 --runs 20 -N`):

| metric | value |
| --- | --- |
| mean (min) | 4.4 ms ± 0.3 ms (3.8 ms min) |
| in-process (`profile`, 100 iters) | ~2.8 ms/iter (~6,100 pages/sec) |
| throughput (CLI) | ~240 MB/s (~3,900 pages/sec) |
| release binary | ~700 KB |

Pages and multi-page Markdown formatting fan out on `std::thread::scope`, fonts are parsed once and cached document-wide, and both the DEFLATE decoder and content-stream tokenizer borrow operands straight from the input bytes.

### Corpus benchmark & text fidelity

Across 10 public PDFs (1,120 pages, 19.6 MB spanning arXiv papers, W3C CSS 2.1, RFC 9110, NIST specs, NASA handbook, IRS W-9, US Constitution):

- **Batch CLI wall time:** `158.8 ms ± 12.1 ms` for all 10 documents (`hyperfine --warmup 3 --runs 20`).
- **Text fidelity vs. Poppler `pdftotext`:** `0.976` mean Token F1 and `0.923` mean reading-order NID, with `0` invalid/replacement characters and `0` unconverted Unicode bullets.

### Published comparison targets

Warmed in-process timings on local synthetic PDFs matching the page counts of commonly published library benchmarks (throughput targets, not claims on identical corpora or hardware):

| document shape | published target | `pdfmd` local run | speedup |
| --- | ---: | ---: | ---: |
| `1_page.pdf` | 2.0 ms | 0.012 ms | ~167x |
| `24_pages.pdf` | 41.0 ms | 0.118 ms | ~347x |
| `60_pages.pdf` | 123.0 ms | 0.201 ms | ~612x |
| `457_pages.pdf` (100 MB) | 777.0 ms | 0.933 ms | ~833x |

## Testing & quality

522 Rust tests (449 unit + 24 integration + 44 end-to-end quality + 5 scorer unit tests) plus 32 Python binding tests, at **99.1% line coverage** (`cargo llvm-cov --all-targets --summary-only --ignore-filename-regex 'src/bin/'`).

```sh
cargo test --all-targets --all-features
cargo test --test quality
cargo build --release --lib && python -m unittest discover -s python/tests
```

Dev-only harnesses live in `src/bin/` behind `--features dev-harness` (excluded from `cargo install`):

```sh
cargo run --release --bin profile --features dev-harness -- tests/fixtures/sample.pdf 100
cargo run --release --bin quality --features dev-harness -- audit tests/fixtures/sample.pdf
cargo run --release --bin quality --features dev-harness -- eval <pdfs-dir> <ref-dir> --min-teds 0.90 --min-nid 0.95
```

`quality` computes NID (reading order), Token F1, TEDS / TEDS-Struct and Cell F1 for GFM tables, MHS for headings, List/Code F1, and structural hygiene audits without external dependencies.

## Architecture

- `src/pdf/` — xref tables/streams, object & object-stream parser, DEFLATE/zlib & stream filters, damaged-file xref repair
- `src/extract/` — encodings, glyph tables, `ToUnicode` CMaps, font cache, streaming content-stream & Form XObject interpreter, image extraction & PNG encoder, tagged-PDF structure tree
- `src/heuristics/` — column banding, ruled & borderless GFM tables, headings, lists, emphasis, running-margin stripping
- `src/lib.rs`, `src/cli.rs`, `src/ffi.rs`, `src/util.rs` — public Rust API, CLI, C ABI (`cdylib`), `std::thread::scope` worker pool

## Limitations

- Targets academic and prose documents; forms, invoices, spanned cells, nested tables, and complex math stay best-effort reflow.
- Fonts without a `ToUnicode` CMap, standard encoding, or `/Differences` array drop glyphs.
- Encrypted PDFs (`PdfError::Encrypted`) and `LZWDecode` streams are unsupported by design. Damaged PDFs (broken xrefs, truncated DEFLATE) are repaired best-effort.

## Contributing & License

Development rules and release steps live in [`AGENTS.md`](AGENTS.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md). Licensed under [MIT](LICENSE).
