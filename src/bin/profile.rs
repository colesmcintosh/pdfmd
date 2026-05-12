//! In-process benchmark loop used for profiling with `samply`.
//!
//! Runs convert_pdf_to_markdown N times back-to-back so the profiler can
//! collect a meaningful number of samples from the hot path. The PDF path
//! and iteration count come from argv.

use std::env;
use std::fs;
use std::process;
use std::time::Instant;

use pdfmd::convert_pdf_to_markdown;

fn main() {
    let mut args = env::args().skip(1);
    let pdf_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: profile <pdf> [iterations]");
        process::exit(2);
    });
    let iterations: usize = args
        .next()
        .map(|s| s.parse().expect("iterations must be an integer"))
        .unwrap_or(100);

    let bytes = fs::read(&pdf_path).expect("read pdf");
    eprintln!(
        "profiling {} iterations of {} ({} KB)",
        iterations,
        pdf_path,
        bytes.len() / 1024
    );

    let start = Instant::now();
    let mut total_md_bytes: usize = 0;
    for _ in 0..iterations {
        let md = convert_pdf_to_markdown(&bytes, false).expect("convert");
        total_md_bytes += md.len();
    }
    let elapsed = start.elapsed();

    eprintln!(
        "{} iters in {:.2?} → {:.1} ms/iter, {} md bytes total",
        iterations,
        elapsed,
        (elapsed.as_secs_f64() * 1000.0) / iterations as f64,
        total_md_bytes
    );
}
