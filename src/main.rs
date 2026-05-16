use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pdfmd::{convert_pdf_to_markdown, ConvertOptions, ExtractedImage};

const HELP: &str = "Convert PDF documents to Markdown.

USAGE:
    pdfmd [OPTIONS] <INPUT>

ARGS:
    <INPUT>    Path to the input PDF file. Use \"-\" to read from stdin.

OPTIONS:
    -o, --output <FILE>             Write Markdown to FILE instead of stdout.
        --page-breaks               Insert `---` between PDF pages.
        --extract-images <DIR>      Save embedded JPEG / JPEG 2000 images
                                    into DIR and reference them inline.
    -h, --help                      Print this help.
    -V, --version                   Print version information.
";

struct Cli {
    input: PathBuf,
    output: Option<PathBuf>,
    page_breaks: bool,
    extract_images: Option<PathBuf>,
}

/// A lightweight argv parser: enough for our four flags, no dependency.
/// Returns `Ok(None)` after handling `--help` / `--version`, in which case
/// the binary exits successfully without doing any work.
fn parse_args() -> Result<Option<Cli>, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut page_breaks = false;
    let mut extract_images: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("pdfmd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--page-breaks" => page_breaks = true,
            "-o" | "--output" => {
                let v = args
                    .next()
                    .ok_or_else(|| "missing value for --output".to_string())?;
                output = Some(PathBuf::from(v));
            }
            v if v.starts_with("--output=") => {
                output = Some(PathBuf::from(&v["--output=".len()..]));
            }
            "--extract-images" => {
                let v = args
                    .next()
                    .ok_or_else(|| "missing value for --extract-images".to_string())?;
                extract_images = Some(PathBuf::from(v));
            }
            v if v.starts_with("--extract-images=") => {
                extract_images = Some(PathBuf::from(&v["--extract-images=".len()..]));
            }
            v if v.starts_with("--") || (v.starts_with('-') && v != "-") => {
                return Err(format!("unknown flag: {v}"));
            }
            // Positional input.
            _ => {
                if input.is_some() {
                    return Err(format!("unexpected positional argument: {arg}"));
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }

    let input = input.ok_or_else(|| "missing <INPUT>".to_string())?;
    Ok(Some(Cli {
        input,
        output,
        page_breaks,
        extract_images,
    }))
}

fn read_input(path: &Path) -> io::Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        fs::read(path)
    }
}

fn write_output(path: Option<&PathBuf>, markdown: &str) -> io::Result<()> {
    match path {
        Some(p) => fs::write(p, markdown),
        None => io::stdout().write_all(markdown.as_bytes()),
    }
}

fn write_images(dir: &Path, images: &[ExtractedImage]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    for img in images {
        fs::write(dir.join(&img.filename), &img.bytes)?;
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let Some(cli) = parse_args().map_err(|e| format!("{e}\n\n{HELP}"))? else {
        return Ok(());
    };

    let bytes = read_input(&cli.input)
        .map_err(|e| format!("failed to read {}: {e}", cli.input.display()))?;

    let image_dir_str = match cli.extract_images.as_ref() {
        Some(p) => Some(p.to_str().ok_or_else(|| {
            format!(
                "--extract-images path must be valid UTF-8 to embed in Markdown: {}",
                p.display()
            )
        })?),
        None => None,
    };

    let opts = ConvertOptions {
        include_page_breaks: cli.page_breaks,
        image_dir: image_dir_str,
    };
    let result = convert_pdf_to_markdown(&bytes, &opts).map_err(|e| e.to_string())?;

    if let Some(dir) = cli.extract_images.as_deref() {
        write_images(dir, &result.images)
            .map_err(|e| format!("failed to write images to {}: {e}", dir.display()))?;
    }
    write_output(cli.output.as_ref(), &result.markdown).map_err(|e| match cli.output.as_ref() {
        Some(p) => format!("failed to write {}: {e}", p.display()),
        None => format!("failed to write to stdout: {e}"),
    })?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_input_from_disk_returns_bytes() {
        let bytes = read_input(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.pdf"
        )))
        .unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn read_input_propagates_io_error() {
        // A path with a NUL byte will error on every supported platform.
        let err = read_input(Path::new("/definitely/missing/file.pdf"));
        assert!(err.is_err());
    }

    #[test]
    fn write_output_to_file_round_trips() {
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-out-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        write_output(Some(&tmp), "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "hello");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn write_images_creates_target_directory_and_writes() {
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-imgs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let images = vec![ExtractedImage {
            filename: "a.jpg".to_string(),
            bytes: vec![1, 2, 3],
        }];
        write_images(&tmp, &images).unwrap();
        assert_eq!(std::fs::read(tmp.join("a.jpg")).unwrap(), vec![1, 2, 3]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_images_with_empty_input_only_creates_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-imgs-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        write_images(&tmp, &[]).unwrap();
        assert!(tmp.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
