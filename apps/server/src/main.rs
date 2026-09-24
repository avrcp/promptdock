#![forbid(unsafe_code)]

use clap::Parser as _;
use promptdock_server::{cli::Cli, run};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli).await {
        eprintln!("promptdock-relay: {error}");
        std::process::exit(1);
    }
}
