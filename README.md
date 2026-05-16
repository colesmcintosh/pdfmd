# pdfmd

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
