mod cache;
mod checked;
mod complete;
mod db;
mod efa;
mod enumerate;
mod fallback;
mod motis;
mod page;
mod plot;
mod rough;
mod search;
mod serve;
mod sync;
mod text;
mod timetable;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod testkit;

use std::io::IsTerminal;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Lists every public transport connection within limits, including those
/// journey planners hide, grouped by first and last train.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    search: Option<search::SearchArgs>,
    #[command(flatten)]
    endpoints: search::Endpoints,
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
#[expect(clippy::print_stdout, clippy::print_stderr, reason = "the command line's report and warnings")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Operator logs on stderr (the journal under systemd), filtered by RUST_LOG.
    // The server logs its jobs by default; the command line shows its report and
    // warnings already, so it logs only when asked.
    let default = if cli.command.is_some() { "info" } else { "off" };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default)))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    if let Some(Command::Serve { addr, parallel, endpoints }) = cli.command {
        return serve::serve(addr, parallel, endpoints).await;
    }
    let args = cli.search.context("FROM and TO are required")?;
    let events = |event: search::Event<'_>| {
        if let search::Event::Warning(warning) = event {
            eprintln!("warning: {warning}");
        }
    };
    let plot = args.html.as_ref().map(|_| search::PlotPage { back: None, details_url: None });
    let outcome = search::run(&args, &cli.endpoints, db::Credentials::load()?, plot, &events).await?;
    print!("{}", outcome.text);
    if let (Some(path), Some(plot)) = (&args.html, &outcome.plot) {
        std::fs::write(path, &plot.html).with_context(|| format!("writing {}", path.display()))?;
        println!("\nPlot written to {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
