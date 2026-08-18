#[cfg(not(test))]
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pdfmd::{convert_pdf_to_markdown, ConvertOptions, ExtractedImage};

#[cfg(not(test))]
const HELP: &str = "Convert PDF documents to Markdown.

USAGE:
    pdfmd [OPTIONS] <INPUT>

ARGS:
    <INPUT>    Path to the input PDF file, an http(s):// URL, or \"-\"
               to read from stdin. URLs are fetched via the `curl`
               command on PATH.

OPTIONS:
    -o, --output <FILE>             Write Markdown to FILE instead of stdout.
        --page-breaks               Insert `---` between PDF pages.
        --extract-images <DIR>      Save supported embedded images into DIR
                                    (JPEG, JPEG 2000, 8-bit rasters as PNG)
                                    and reference them inline.
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
#[cfg(not(test))]
fn parse_args() -> Result<Option<Cli>, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut page_breaks = false;
    let mut extract_images: Option<PathBuf> = None;

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == OsStr::new("-h") || arg == OsStr::new("--help") {
            print!("{HELP}");
            return Ok(None);
        } else if arg == OsStr::new("-V") || arg == OsStr::new("--version") {
            println!("pdfmd {}", env!("CARGO_PKG_VERSION"));
            return Ok(None);
        } else if arg == OsStr::new("--page-breaks") {
            page_breaks = true;
        } else if arg == OsStr::new("-o") || arg == OsStr::new("--output") {
            let v = args
                .next()
                .ok_or_else(|| "missing value for --output".to_string())?;
            output = Some(PathBuf::from(v));
        } else if arg == OsStr::new("--extract-images") {
            let v = args
                .next()
                .ok_or_else(|| "missing value for --extract-images".to_string())?;
            extract_images = Some(PathBuf::from(v));
        } else {
            // Flags are always valid UTF-8; a path that isn't can only be the
            // positional input.
            if let Some(s) = arg.to_str() {
                if let Some(rest) = s.strip_prefix("--output=") {
                    output = Some(PathBuf::from(rest));
                    continue;
                }
                if let Some(rest) = s.strip_prefix("--extract-images=") {
                    extract_images = Some(PathBuf::from(rest));
                    continue;
                }
                if s.starts_with("--") || (s.starts_with('-') && s != "-") {
                    return Err(format!("unknown flag: {s}"));
                }
            }
            if input.is_some() {
                return Err(format!(
                    "unexpected positional argument: {}",
                    arg.to_string_lossy()
                ));
            }
            input = Some(PathBuf::from(arg));
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
    let mut stdin = io::stdin();
    read_input_with_stdin(path, &mut stdin)
}

fn read_input_with_stdin(path: &Path, stdin: &mut dyn Read) -> io::Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        return read_all(stdin);
    }
    if let Some(url) = path.to_str().filter(|s| is_url(s)) {
        return fetch_url(url);
    }
    fs::read(path)
}

fn read_all(reader: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    Ok(buf)
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Fetch a URL by shelling out to `curl`. Staying out of the Rust HTTP /
/// TLS ecosystem keeps the crate's zero-dependency promise; the cost is a
/// runtime dependency on `curl`, which is universally available on Linux
/// / macOS and shipped with modern Windows.
fn fetch_url(url: &str) -> io::Result<Vec<u8>> {
    fetch_url_with(Command::new("curl"), url)
}

fn fetch_url_with(mut cmd: Command, url: &str) -> io::Result<Vec<u8>> {
    let output = cmd
        .args([
            "--fail",       // non-2xx -> curl exits non-zero
            "--silent",     // suppress progress meter on stderr
            "--show-error", // but still print actual error messages
            "--location",   // follow redirects
            "--max-time",
            "120",
            "--user-agent",
            // Some CDNs reject the default curl UA. A neutral browser-style
            // string gets us past those without changing behaviour on plain
            // servers.
            "Mozilla/5.0 (compatible; pdfmd)",
            "--",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("could not run `curl` to fetch {url}: {e}"),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let trimmed = stderr.trim();
        let detail = if trimmed.is_empty() {
            format!("curl exited with status {}", output.status)
        } else {
            trimmed.to_string()
        };
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("fetch failed for {url}: {detail}"),
        ));
    }
    Ok(output.stdout)
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

#[cfg(not(test))]
pub(crate) fn run() -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
    }

    /// Collision-free path under the system temp dir, tagged for readability.
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

    fn cli(input: PathBuf, output: Option<PathBuf>, extract_images: Option<PathBuf>) -> Cli {
        Cli {
            input,
            output,
            page_breaks: false,
            extract_images,
        }
    }

    #[test]
    fn is_url_recognises_http_and_https_only() {
        assert!(is_url("http://example.com/x.pdf"));
        assert!(is_url("https://example.com/x.pdf"));
        assert!(!is_url("file:///tmp/x.pdf"));
        assert!(!is_url("ftp://example.com/x.pdf"));
        assert!(!is_url("/tmp/x.pdf"));
        assert!(!is_url("-"));
        assert!(!is_url(""));
    }

    #[test]
    fn fetch_url_reports_curl_error_for_unresolvable_host() {
        // A `.invalid` host is reserved by RFC 6761 and will never resolve.
        // We're checking the error-surface shape, not the network — so this
        // test passes regardless of outbound connectivity.
        let err = fetch_url("https://nonexistent.invalid/x.pdf").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("fetch failed"));
        assert!(msg.contains("nonexistent.invalid"));
    }

    #[test]
    fn read_input_routes_urls_through_fetch() {
        let err = read_input(Path::new("https://nonexistent.invalid/x.pdf")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("fetch failed"));
        assert!(msg.contains("nonexistent.invalid"));
    }

    #[test]
    fn read_input_dash_reads_supplied_stdin() {
        let mut stdin: &[u8] = b"%PDF-1.4";
        let bytes = read_input_with_stdin(Path::new("-"), &mut stdin).unwrap();
        assert_eq!(bytes, b"%PDF-1.4");
    }

    #[test]
    fn fetch_url_reports_missing_curl() {
        let err = fetch_url_with(
            Command::new("pdfmd-definitely-not-a-real-curl-binary"),
            "http://example.invalid/x.pdf",
        )
        .unwrap_err();
        assert!(err.to_string().contains("could not run"));
    }

    #[cfg(unix)]
    #[test]
    fn fetch_url_reports_empty_stderr_status() {
        let err =
            fetch_url_with(Command::new("false"), "http://example.invalid/x.pdf").unwrap_err();
        assert!(err.to_string().contains("curl exited with status"));
    }

    #[cfg(unix)]
    #[test]
    fn fetch_url_returns_stdout_on_success() {
        let path = tmp_path("curl-file");
        std::fs::write(&path, b"%PDF-1.4").unwrap();
        let url = format!("file://{}", path.display());
        let bytes = fetch_url(&url).unwrap();
        assert_eq!(bytes, b"%PDF-1.4");
        let _ = std::fs::remove_file(&path);
    }

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
    fn read_all_propagates_reader_error() {
        struct FailingRead;

        impl Read for FailingRead {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::Other, "boom"))
            }
        }

        let mut reader = FailingRead;
        let err = read_all(&mut reader).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn read_all_returns_reader_bytes() {
        let mut reader: &[u8] = b"%PDF-1.4";
        let bytes = read_all(&mut reader).unwrap();
        assert_eq!(bytes, b"%PDF-1.4");
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
        let tmp = tmp_path("imgs");
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
        let tmp = tmp_path("imgs-blocked");
        std::fs::write(&tmp, b"not a dir").unwrap();
        let err = write_images(&tmp, &[]).unwrap_err();
        assert!(!err.to_string().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn write_images_errors_when_writing_into_unwritable_path() {
        // Create the dir, then create a *directory* (not a file) at the
        // path where we'd try to write the image — that turns the inner
        // `fs::write` into an error.
        let tmp = tmp_path("imgs-busy");
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
        let tmp = tmp_path("out-dir");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(write_output(Some(&tmp), "x").is_err());
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn execute_writes_markdown_and_images_when_dir_set() {
        let out = tmp_path("md");
        let imgs = tmp_path("imgs");
        execute(cli(fixture(), Some(out.clone()), Some(imgs.clone()))).expect("execute");
        let md = std::fs::read_to_string(&out).expect("read out");
        assert!(!md.is_empty());
        assert!(imgs.exists());
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&imgs);
    }

    #[test]
    fn execute_propagates_input_read_error() {
        let missing = PathBuf::from("/definitely/missing/pdfmd-exec.pdf");
        let err = execute(cli(missing, None, None)).unwrap_err();
        assert!(err.contains("failed to read"));
    }

    #[test]
    fn execute_propagates_convert_error_for_non_pdf() {
        let path = tmp_path("notpdf");
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(execute(cli(path.clone(), None, None)).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_propagates_write_images_error() {
        let out = tmp_path("md");
        let imgs_blocker = tmp_path("imgs-blocked");
        // Place a regular file where the images dir would go.
        std::fs::write(&imgs_blocker, b"i am a file").unwrap();
        let err = execute(cli(
            fixture(),
            Some(out.clone()),
            Some(imgs_blocker.clone()),
        ))
        .unwrap_err();
        assert!(err.contains("failed to write images"));
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&imgs_blocker);
    }

    #[test]
    fn execute_propagates_write_output_error_for_unwritable_path() {
        let out_dir = tmp_path("out-as-dir");
        std::fs::create_dir_all(&out_dir).unwrap();
        let err = execute(cli(fixture(), Some(out_dir.clone()), None)).unwrap_err();
        assert!(err.contains("failed to write"));
        let _ = std::fs::remove_dir(&out_dir);
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_non_utf8_extract_images_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        // Lone 0xFF byte → not valid UTF-8 on Unix paths.
        let bad = PathBuf::from(OsString::from_vec(vec![0xFF]));
        let err = execute(cli(fixture(), Some(tmp_path("md")), Some(bad))).unwrap_err();
        assert!(err.contains("must be valid UTF-8"));
    }

    #[test]
    fn execute_writes_to_stdout_when_no_output_given() {
        // We can't easily capture stdout in-process, but we can at least
        // run the code path. write_output → io::stdout().write_all should
        // succeed in the test harness.
        // Don't assert anything about output content — just that the call
        // doesn't error.
        execute(cli(fixture(), None, None)).expect("execute to stdout");
    }

    #[test]
    fn write_images_with_empty_input_only_creates_dir() {
        let tmp = tmp_path("imgs-empty");
        write_images(&tmp, &[]).unwrap();
        assert!(tmp.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
