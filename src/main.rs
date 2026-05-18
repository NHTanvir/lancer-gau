//! lancer-gau
//! Rust clone of gau. See plan.md (local, gitignored) for the spec.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "lancer-gau", version, about = "Rust clone of gau")]
struct Cli {}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let _cli = Cli::parse();
    println!("lancer-gau - scaffold ready. Implement per plan.md.");
    Ok(())
}
