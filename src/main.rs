use anyhow::Result;
use clap::Parser;
use putio::run;
use putio::Config;

fn main() -> Result<()> {
    env_logger::init();

    run(Config::parse())
}
