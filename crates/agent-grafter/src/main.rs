mod cli;
mod manifest;

fn main() -> std::process::ExitCode {
    cli::run().into()
}
