use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use rust_parse::convert_pdf_to_markdown;

#[derive(Parser, Debug)]
#[command(
    name = "rust-parse",
    version,
    about = "Convert PDF documents to Markdown",
    long_about = None,
)]
struct Cli {
    /// Path to the input PDF file. Use "-" to read from stdin.
    input: PathBuf,

    /// Path to write the Markdown output. Defaults to stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Insert a page break marker (`---`) between PDF pages.
    #[arg(long, default_value_t = false)]
    page_breaks: bool,
}

fn read_input(path: &PathBuf) -> Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read PDF from stdin")?;
        Ok(buf)
    } else {
        fs::read(path).with_context(|| format!("failed to read {}", path.display()))
    }
}

fn write_output(path: Option<&PathBuf>, markdown: &str) -> Result<()> {
    match path {
        Some(p) => {
            fs::write(p, markdown).with_context(|| format!("failed to write {}", p.display()))
        }
        None => io::stdout()
            .write_all(markdown.as_bytes())
            .context("failed to write to stdout"),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let bytes = read_input(&cli.input)?;
    let markdown = convert_pdf_to_markdown(&bytes, cli.page_breaks)?;
    write_output(cli.output.as_ref(), &markdown)?;
    Ok(())
}
