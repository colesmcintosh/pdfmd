use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Parser;

use pdfmd::{convert_pdf_to_markdown, ConvertOptions, ExtractedImage};

#[derive(Parser, Debug)]
#[command(
    name = "pdfmd",
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

    /// Extract embedded JPEG / JPEG 2000 images into the given directory
    /// and reference them inline in the Markdown.
    #[arg(long, value_name = "DIR")]
    extract_images: Option<PathBuf>,
}

fn read_input(path: &Path) -> Result<Vec<u8>> {
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

fn write_images(dir: &Path, images: &[ExtractedImage]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    for img in images {
        let path = dir.join(&img.filename);
        fs::write(&path, &img.bytes)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let bytes = read_input(&cli.input)?;

    let image_dir_str = cli
        .extract_images
        .as_ref()
        .map(|p| {
            p.to_str().ok_or_else(|| {
                anyhow!(
                    "--extract-images path must be valid UTF-8 to embed in Markdown: {}",
                    p.display()
                )
            })
        })
        .transpose()?;

    let opts = ConvertOptions {
        include_page_breaks: cli.page_breaks,
        image_dir: image_dir_str,
    };
    let result = convert_pdf_to_markdown(&bytes, &opts)?;

    if let Some(dir) = cli.extract_images.as_deref() {
        write_images(dir, &result.images)?;
    }
    write_output(cli.output.as_ref(), &result.markdown)?;
    Ok(())
}
