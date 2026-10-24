mod cache;
mod db;
mod efa;
mod enumerate;
mod fallback;
mod motis;
mod plot;
mod rough;
mod search;
mod serve;
mod text;
mod timetable;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

/// Lists every public transport connection within limits, including those
/// journey planners hide, grouped by first and last train.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    search: Option<search::SearchArgs>,
}

#[derive(Subcommand)]
enum Command {
    /// Route planner in the browser: a form for every option, results as the plot.
    Serve {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
        /// Searches run at the same time; more wait their turn (they load
        /// Transitous and the DB quota).
        #[arg(long, default_value_t = 1)]
        parallel: usize,
        /// Services and contact used by every search; not settable from the page.
        #[command(flatten)]
        endpoints: search::Endpoints,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Serve { addr, parallel, endpoints }) = cli.command {
        return serve::serve(addr, parallel, endpoints).await;
    }
    let args = cli.search.context("FROM and TO are required")?;
    let events = |event: search::Event<'_>| {
        if let search::Event::Warning(warning) = event {
            eprintln!("warning: {warning}");
        }
    };
    let outcome = search::run(&args, args.html.as_ref().map(|_| None), &events).await?;
    print!("{}", outcome.text);
    if let (Some(path), Some(html)) = (&args.html, &outcome.html) {
        std::fs::write(path, html).with_context(|| format!("writing {}", path.display()))?;
        println!("\nPlot written to {}", path.display());
    }
    Ok(())
}
