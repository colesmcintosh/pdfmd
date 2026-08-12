//! Batch-convert a directory of PDFs to Markdown files named `{stem}.md`.
//!
//! Used to score pdfmd against OpenDataLoader-style corpora (NID / TEDS / MHS)
//! without pulling an HTTP client or extra crates into the library.

use std::env;
use std::fs;
use std::path::Path;
use std::process;

use pdfmd::{convert_pdf_to_markdown, ConvertOptions};

fn main() {
    let mut args = env::args().skip(1);
    let pdfs_dir = args.next().unwrap_or_else(usage);
    let out_dir = args.next().unwrap_or_else(usage);

    let pdfs = Path::new(&pdfs_dir);
    let out = Path::new(&out_dir);
    if !pdfs.is_dir() {
        eprintln!("error: {pdfs_dir} is not a directory");
        process::exit(2);
    }
    fs::create_dir_all(out).unwrap_or_else(|e| {
        eprintln!("error: create {out_dir}: {e}");
        process::exit(1);
    });

    let opts = ConvertOptions::default();
    let mut converted = 0usize;
    let mut failed = 0usize;
    let mut entries: Vec<_> = fs::read_dir(pdfs)
        .unwrap_or_else(|e| {
            eprintln!("error: read {pdfs_dir}: {e}");
            process::exit(1);
        })
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let is_pdf = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);
        if !is_pdf {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
        let dest = out.join(format!("{stem}.md"));
        match fs::read(&path).and_then(|bytes| {
            convert_pdf_to_markdown(&bytes, &opts)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
        }) {
            Ok(result) => {
                if let Err(e) = fs::write(&dest, result.markdown) {
                    eprintln!("error: write {}: {e}", dest.display());
                    failed += 1;
                } else {
                    converted += 1;
                }
            }
            Err(e) => {
                eprintln!("error: {}: {e}", path.display());
                failed += 1;
            }
        }
    }

    eprintln!("converted {converted}, failed {failed}");
    if failed > 0 {
        process::exit(1);
    }
}

fn usage() -> String {
    eprintln!("usage: opendataloader <pdfs-dir> <out-dir>");
    process::exit(2);
}
