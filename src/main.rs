use std::env;
use std::io::{self, Result, Write};
use std::process;

use putio::run;
use putio::Config;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let stdout = io::stdout();

    let config = Config::new(&args).unwrap_or_else(|err| {
        let mut handle = stdout.lock();
        writeln!(handle, "\r\nProblem parsing arguments: {}\r\n", err).unwrap();
        process::exit(1);
    });

    if let Err(err) = run(config) {
        let mut handle = stdout.lock();
        writeln!(handle, "\r\nError: {}\r\n", err).unwrap();

        process::exit(1);
    }
    Ok(())
}
