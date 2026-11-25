//! popier: watch GitHub Actions pipelines of many repositories on one screen,
//! with every workflow, job and step laid out as a tree.

mod config;
mod github;
mod snapshot;
mod status;
mod tree;
mod tui;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::snapshot::ExpandRequest;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file (default: ~/.config/popier/config.toml).
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the interactive dashboard (default).
    Tui,
    /// Print the tree once and exit, like the original bash prototype.
    Once {
        /// Expand every workflow, not only running/queued/failed ones.
        #[arg(long)]
        all: bool,
        /// Disable ANSI colors.
        #[arg(long)]
        no_color: bool,
    },
    /// Write a sample config file.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match cli.config {
        Some(p) => p,
        None => Config::default_path().context("cannot determine the config directory")?,
    };

    match cli.command.unwrap_or(Cmd::Tui) {
        Cmd::Init { force } => init(&path, force),
        Cmd::Once { all, no_color } => {
            let config = Config::load(&path)?;
            let client = github::Client::new(github::resolve_token()?)?;
            let mut expand = ExpandRequest::from_mode(config.settings.expand);
            expand.all |= all;
            let snap = snapshot::take(&client, &config, &expand).await;
            let color = !no_color && std::io::stdout().is_terminal();
            print!(
                "{}",
                tree::render_plain(&tree::build(&snap, &tree::Folds::default()), color)
            );
            Ok(())
        }
        Cmd::Tui => {
            let config = Config::load(&path)?;
            let client = github::Client::new(github::resolve_token()?)?;
            tui::run(config, client).await
        }
    }
}

fn init(path: &PathBuf, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, config::SAMPLE)?;
    println!("wrote {}", path.display());
    Ok(())
}
