mod character;
mod checks;
mod claude;
mod engine;
mod pace;
mod protocol;
mod reply;
mod scratch;
mod selftest;
mod serve;
mod stub;
mod voice;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => match serve::run(&args[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vrchatcore: {e}");
                ExitCode::FAILURE
            }
        },
        Some("selftest") => selftest::run(),
        // The flags the engine starts `claude` with. The self-test points the engine at this
        // executable instead, so the process handling is tested against something that costs
        // nothing.
        Some("-p") => stub::run(),
        other => {
            eprintln!("vrchatcore: expected 'serve' or 'selftest', got {other:?}");
            ExitCode::from(2)
        }
    }
}
