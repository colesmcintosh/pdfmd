mod cli;

#[cfg(not(test))]
use std::process::ExitCode;

#[cfg(not(test))]
fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}
