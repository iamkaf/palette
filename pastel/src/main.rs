use std::process::ExitCode;

fn main() -> ExitCode {
    let Ok(args) = std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<String>, _>>()
    else {
        eprintln!("pastel: arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };
    match pastel::cli::run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.is_explained() {
                // Fallback for errors that bypassed friendly formatting.
                eprintln!("pastel: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
