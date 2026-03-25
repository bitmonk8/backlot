// vault CLI: thin binary over the vault library.

use clap::Parser;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "vault", about = "Vault")]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    ExitCode::SUCCESS
}
