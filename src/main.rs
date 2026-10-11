//! The `kargo` command-line tool.

mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::main()
}
