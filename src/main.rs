//! popier: watch GitHub Actions pipelines of many repositories on one screen,
//! with every workflow, job and step laid out as a tree.

mod config;
mod detail;
mod discover;
mod filter;
mod github;
mod logs;
mod notify;
mod paint;
mod snapshot;
mod status;
mod theme;
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
    /// Write a config file: a sample, or your own repos with --discover.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        /// Find your repositories through your GitHub login (`gh`), keep the
        /// ones with active workflows (no archived repos or forks), and group
        /// repos that share a name prefix, like brabo, brabo-api, brabo-web.
        #[arg(long)]
        discover: bool,
        /// With --discover: only repositories of this user or organization.
        /// Repeat for more than one.
        #[arg(long, value_name = "OWNER", requires = "discover")]
        owner: Vec<String>,
        /// Print the config instead of writing it.
        #[arg(long)]
        print: bool,
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
        Cmd::Init {
            force,
            discover,
            owner,
            print,
        } => {
            if !print && path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite, or --print to see the result)",
                    path.display()
                );
            }
            let text = if discover {
                discover_config(&owner).await?
            } else {
                config::SAMPLE.to_owned()
            };
            if print {
                print!("{text}");
                Ok(())
            } else {
                write_config(&path, &text)
            }
        }
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

async fn discover_config(owners: &[String]) -> Result<String> {
    let client = github::Client::new(github::resolve_token()?)?;
    eprintln!("looking for your repositories on GitHub...");
    let report = discover::discover(&client, owners).await?;
    let scope = if owners.is_empty() {
        "visible to you".to_owned()
    } else {
        format!("in {}", owners.join(", "))
    };
    eprintln!(
        "{} repositories {scope}, {} archived or forks skipped, {} without active workflows",
        report.seen, report.skipped_archived_or_fork, report.without_workflows
    );
    if report.found.is_empty() {
        bail!("no repository with active workflows found");
    }
    let (groups, _) = discover::suggest_groups(&report.found);
    eprintln!(
        "{} repositories to watch, {} groups{}",
        report.found.len(),
        groups.len(),
        if groups.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                groups
                    .iter()
                    .map(|(name, m)| format!("{name}: {}", m.len()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    Ok(discover::render(&report.found))
}

fn write_config(path: &PathBuf, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    println!("wrote {}", path.display());
    Ok(())
}
