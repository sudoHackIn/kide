/// Process entry point: all CLI behavior lives in the library crate.
fn main() -> std::process::ExitCode {
    kide::run_cli()
}
