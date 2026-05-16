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
    execute(cli)
}

/// Drive the conversion for an already-parsed `Cli`. Split out from `run`
/// so the tests can exercise the orchestration without touching
/// `std::env::args` or stdin.
fn execute(cli: Cli) -> Result<(), String> {
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
    fn write_images_errors_when_dir_path_is_a_file() {
        // Block dir creation by placing a regular file at the target path.
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-imgs-blocked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::write(&tmp, b"not a dir").unwrap();
        let err = write_images(&tmp, &[]).unwrap_err();
        // The exact OS error message varies, but the call must fail.
        assert!(err.kind() != std::io::ErrorKind::Other || !err.to_string().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn write_images_errors_when_writing_into_unwritable_path() {
        // Create the dir, then create a *directory* (not a file) at the
        // path where we'd try to write the image — that turns the inner
        // `fs::write` into an error.
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-imgs-busy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let blocker = tmp.join("a.jpg");
        std::fs::create_dir(&blocker).unwrap();
        let images = vec![ExtractedImage {
            filename: "a.jpg".into(),
            bytes: vec![1, 2, 3],
        }];
        assert!(write_images(&tmp, &images).is_err());
        let _ = std::fs::remove_dir(&blocker);
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn write_output_errors_when_target_is_unwritable() {
        // A directory at the target path makes fs::write fail.
        let tmp = std::env::temp_dir().join(format!(
            "pdfmd-out-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(write_output(Some(&tmp), "x").is_err());
        let _ = std::fs::remove_dir(&tmp);
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
    }

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pdfmd-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ))
    }

    #[test]
    fn execute_writes_markdown_and_images_when_dir_set() {
        let out = tmp_path("md");
        let imgs = tmp_path("imgs");
        let cli = Cli {
            input: fixture(),
            output: Some(out.clone()),
            page_breaks: false,
            extract_images: Some(imgs.clone()),
        };
        execute(cli).expect("execute");
        let md = std::fs::read_to_string(&out).expect("read out");
        assert!(!md.is_empty());
        assert!(imgs.exists());
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&imgs);
    }

    #[test]
    fn execute_propagates_input_read_error() {
        let cli = Cli {
            input: PathBuf::from("/definitely/missing/pdfmd-exec.pdf"),
            output: None,
            page_breaks: false,
            extract_images: None,
        };
        let err = execute(cli).unwrap_err();
        assert!(err.contains("failed to read"));
    }

    #[test]
    fn execute_propagates_convert_error_for_non_pdf() {
        let path = tmp_path("notpdf");
        std::fs::write(&path, b"not a pdf").unwrap();
        let cli = Cli {
            input: path.clone(),
            output: None,
            page_breaks: false,
            extract_images: None,
        };
        assert!(execute(cli).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_propagates_write_images_error() {
        let out = tmp_path("md");
        let imgs_blocker = tmp_path("imgs-blocked");
        // Place a regular file where the images dir would go.
        std::fs::write(&imgs_blocker, b"i am a file").unwrap();
        let cli = Cli {
            input: fixture(),
            output: Some(out.clone()),
            page_breaks: false,
            extract_images: Some(imgs_blocker.clone()),
        };
        let err = execute(cli).unwrap_err();
        assert!(err.contains("failed to write images"));
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&imgs_blocker);
    }

    #[test]
    fn execute_propagates_write_output_error_for_unwritable_path() {
        let out_dir = tmp_path("out-as-dir");
        std::fs::create_dir_all(&out_dir).unwrap();
        let cli = Cli {
            input: fixture(),
            output: Some(out_dir.clone()),
            page_breaks: false,
            extract_images: None,
        };
        let err = execute(cli).unwrap_err();
        assert!(err.contains("failed to write"));
        let _ = std::fs::remove_dir(&out_dir);
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_non_utf8_extract_images_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let cli = Cli {
            input: fixture(),
            output: Some(tmp_path("md")),
            page_breaks: false,
            // Lone 0xFF byte → not valid UTF-8 on Unix paths.
            extract_images: Some(PathBuf::from(OsString::from_vec(vec![0xFF]))),
        };
        let err = execute(cli).unwrap_err();
        assert!(err.contains("must be valid UTF-8"));
    }

    #[test]
    fn execute_writes_to_stdout_when_no_output_given() {
        // We can't easily capture stdout in-process, but we can at least
        // run the code path. write_output → io::stdout().write_all should
        // succeed in the test harness.
        let cli = Cli {
            input: fixture(),
            output: None,
            page_breaks: false,
            extract_images: None,
        };
        // Don't assert anything about output content — just that the call
        // doesn't error.
        execute(cli).expect("execute to stdout");
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
