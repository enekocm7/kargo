//! The `kargo` command-line tool.

mod cli;
mod complete;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::main()
}
