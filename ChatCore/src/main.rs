mod character;
mod chat;
mod checks;
mod claude;
mod dialogue;
mod engine;
mod pace;
mod protocol;
mod relay;
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
        // The models the chat service has, for the settings window, which cannot reach it itself.
        Some("models") => chat::print_models(),
        // The courier's gate, run by `claude` as a hook before each of the courier's tool calls.
        Some("relay-gate") => relay::gate_main(&args[2..]),
        // The courier's stop, run by `claude` as a hook after each of the courier's tool calls.
        Some("relay-stop") => relay::stop_main(),
        // The flags the engine starts `claude` with. The self-test points the engine at this
        // executable instead, so the process handling is tested against something that costs
        // nothing.
        Some("-p") => stub::run(),
        other => {
            eprintln!("vrchatcore: expected 'serve', 'selftest' or 'models', got {other:?}");
            ExitCode::from(2)
        }
    }
}
