use anyhow::Result;
use clap::Parser;
use putio::run;
use putio::Config;

fn main() -> Result<()> {
    env_logger::init();

    // `run` resolves the token itself, so `putio --login` works on a machine
    // that does not have one yet.
    run(Config::parse())
}
