use std::env;
use std::process;

use putio::run;
use putio::Config;

fn main() {
    let args: Vec<String> = env::args().collect();

    let config = Config::new(&args).unwrap_or_else(|err| {
        println!("\r\nProblems parsing arguments: {}\r\n", err);
        process::exit(1);
    });

    if let Err(err) = run(config) {
        println!("\r\nError: {}\r\n", err);

        process::exit(1);
    }
}
