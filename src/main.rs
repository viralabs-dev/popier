//! popier: watch GitHub Actions pipelines of many repositories on one screen,
//! with every workflow, job and step laid out as a tree.

mod config;
mod detail;
mod discover;
mod failures;
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
mod update;

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

    /// Update popier to the latest release (checks its SHA-256 first).
    #[arg(long, exclusive = true)]
    update: bool,

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
        /// After the tree, print the end of the log of every failing step.
        #[arg(long)]
        logs: bool,
        /// With --logs: how many lines per failing step.
        #[arg(long, value_name = "N", default_value_t = failures::DEFAULT_LINES, requires = "logs")]
        log_lines: usize,
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
        /// With --discover: only public repositories (handy for a demo or a
        /// screen you share).
        #[arg(long, requires = "discover")]
        public: bool,
        /// Print the config instead of writing it.
        #[arg(long)]
        print: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.update {
        return update::run().await;
    }
    let path = match cli.config {
        Some(p) => p,
        None => Config::default_path().context("cannot determine the config directory")?,
    };

    match cli.command.unwrap_or(Cmd::Tui) {
        Cmd::Init {
            force,
            discover,
            owner,
            public,
            print,
        } => {
            if !print && path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite, or --print to see the result)",
                    path.display()
                );
            }
            let text = if discover {
                discover_config(&owner, public).await?
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
        Cmd::Once {
            all,
            no_color,
            logs,
            log_lines,
        } => {
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
            if logs {
                let found = failures::collect(&snap);
                print!(
                    "{}",
                    failures::render(&client, &found, log_lines, color).await
                );
            }
            Ok(())
        }
        Cmd::Tui => {
            let config = Config::load(&path)?;
            let client = github::Client::new(github::resolve_token()?)?;
            tui::run(config, client).await
        }
    }
}

async fn discover_config(owners: &[String], public_only: bool) -> Result<String> {
    let client = github::Client::new(github::resolve_token()?)?;
    eprintln!("looking for your repositories on GitHub...");
    let report = discover::discover(&client, owners, public_only).await?;
    let scope = if owners.is_empty() {
        "visible to you".to_owned()
    } else {
        format!("in {}", owners.join(", "))
    };
    let kind = if public_only {
        "public repositories"
    } else {
        "repositories"
    };
    eprintln!(
        "{} {kind} {scope}, {} archived or forks skipped, {} without active workflows",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `popier --update` is how installed copies get new releases: every
    /// version must keep it, or users stay stuck on that version.
    #[test]
    fn update_flag_is_always_there() {
        let cli = Cli::try_parse_from(["popier", "--update"]).unwrap();
        assert!(cli.update);
        assert!(cli.command.is_none());
    }
}
