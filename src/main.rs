use std::process;

use clap::Parser;
use putio::run;
use putio::Config;

fn main() {
    env_logger::init();

    let mut config = Config::parse();
    if let Err(err) = config.resolve_token() {
        eprintln!("Problem parsing arguments: {}", err);
        process::exit(1);
    }

    if let Err(err) = run(config) {
        eprintln!("Error: {}", err);
        process::exit(1);
    }
}
