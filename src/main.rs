use std::io::{self, Result, Write};
use std::process;

use clap::Parser;
use putio::run;
use putio::Config;

fn main() -> Result<()> {
    env_logger::init();
    let stdout = io::stdout();

    let mut config = Config::parse();
    if let Err(err) = config.resolve_token() {
        let mut handle = stdout.lock();
        writeln!(handle, "\r\nProblem parsing arguments: {}\r\n", err).unwrap();
        process::exit(1);
    }

    if let Err(err) = run(config) {
        let mut handle = stdout.lock();
        writeln!(handle, "\r\nError: {}\r\n", err).unwrap();

        process::exit(1);
    }
    Ok(())
}
