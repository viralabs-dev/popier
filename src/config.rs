//! Configuration file: which repositories to watch and how.
//!
//! Default location: `$XDG_CONFIG_HOME/popier/config.toml`
//! (usually `~/.config/popier/config.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::theme::BarStyle;

/// Sample config written by `popier init`.
pub const SAMPLE: &str = include_str!("../examples/config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    /// Repositories to watch, in display order.
    #[serde(rename = "repo", default)]
    pub repos: Vec<RepoConfig>,
    /// Named groups of repositories (a project with many repos), in display order.
    #[serde(rename = "group", default)]
    pub groups: Vec<GroupConfig>,
}

/// A project made of several repositories, shown as one collapsible row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupConfig {
    pub name: String,
    /// Slugs of member repositories. A slug without its own `[[repo]]` entry
    /// is watched with default options.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// Seconds between refreshes. Keep it >= 30 to stay well under the API rate limit.
    pub refresh_secs: u64,
    /// Which workflows start expanded: only active/failed ones, or all of them.
    pub expand: ExpandMode,
    /// How many recent runs to scan per repo when picking the latest run of each workflow.
    pub runs_per_repo: u8,
    /// Desktop notification (`notify-send`) when a workflow starts failing or recovers.
    pub notify: bool,
    /// Only notify for these branches. Empty means every branch.
    pub notify_branches: Vec<String>,
    /// Color theme: a built-in (`noite`, `brasa`, `papel`, `alto-contraste`,
    /// `terminal`) or `~/.config/popier/themes/<name>.toml`. Unset picks
    /// `noite`, or `terminal` when the terminal lacks true color.
    pub theme: Option<String>,
    /// Progress bar style: `line` or `block`.
    pub bar: BarStyle,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_secs: 60,
            expand: ExpandMode::Attention,
            runs_per_repo: 30,
            notify: true,
            notify_branches: Vec::new(),
            theme: None,
            bar: BarStyle::Line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpandMode {
    /// Expand workflows that are running, queued or failed.
    Attention,
    /// Expand every workflow.
    All,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// `owner/name`, e.g. `viralabs-dev/brabo`.
    pub slug: String,
    /// Optional label shown instead of the slug.
    pub label: Option<String>,
    /// Only consider runs on this branch.
    pub branch: Option<String>,
}

impl RepoConfig {
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.slug)
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("popier").join("config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config file {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(text)?;
        config.add_group_members();
        config.validate()?;
        Ok(config)
    }

    /// Group of a repository, if any.
    pub fn group_of(&self, slug: &str) -> Option<&GroupConfig> {
        self.groups
            .iter()
            .find(|g| g.repos.iter().any(|r| r == slug))
    }

    /// Group members listed only inside a `[[group]]` become plain repos.
    fn add_group_members(&mut self) {
        for group in &self.groups {
            for slug in &group.repos {
                if !self.repos.iter().any(|r| &r.slug == slug) {
                    self.repos.push(RepoConfig {
                        slug: slug.clone(),
                        label: None,
                        branch: None,
                    });
                }
            }
        }
    }

    fn validate(&self) -> Result<()> {
        if self.repos.is_empty() {
            bail!(
                "no repositories configured: add a [[repo]] entry, or run `popier init --discover`"
            );
        }
        let mut seen = std::collections::HashSet::new();
        for group in &self.groups {
            if group.name.trim().is_empty() {
                bail!("every [[group]] needs a name");
            }
            for slug in &group.repos {
                if !seen.insert(slug.as_str()) {
                    bail!("repo {slug:?} is in more than one [[group]]");
                }
            }
        }
        for repo in &self.repos {
            let parts: Vec<&str> = repo.slug.split('/').collect();
            if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
                bail!(
                    "repo slug must look like \"owner/name\", got {:?}",
                    repo.slug
                );
            }
        }
        if self.settings.refresh_secs < 5 {
            bail!("settings.refresh_secs must be at least 5");
        }
        if !(1..=100).contains(&self.settings.runs_per_repo) {
            bail!("settings.runs_per_repo must be between 1 and 100");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_config_is_valid() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.settings.expand, ExpandMode::Attention);
    }

    #[test]
    fn defaults_apply_when_settings_missing() {
        let config = Config::parse("[[repo]]\nslug = \"a/b\"\n").unwrap();
        assert_eq!(config.settings.refresh_secs, 60);
        assert_eq!(config.repos[0].display_name(), "a/b");
    }

    #[test]
    fn notify_settings() {
        let config = Config::parse("[[repo]]\nslug = \"a/b\"\n").unwrap();
        assert!(config.settings.notify);
        assert!(config.settings.notify_branches.is_empty());
        let config = Config::parse(
            "[settings]\nnotify = false\nnotify_branches = [\"main\"]\n[[repo]]\nslug = \"a/b\"\n",
        )
        .unwrap();
        assert!(!config.settings.notify);
        assert_eq!(config.settings.notify_branches, vec!["main"]);
    }

    #[test]
    fn theme_settings() {
        let config = Config::parse("[[repo]]\nslug = \"a/b\"\n").unwrap();
        assert_eq!(config.settings.theme, None);
        assert_eq!(config.settings.bar, BarStyle::Line);
        let config = Config::parse(
            "[settings]\ntheme = \"brasa\"\nbar = \"block\"\n[[repo]]\nslug = \"a/b\"\n",
        )
        .unwrap();
        assert_eq!(config.settings.theme.as_deref(), Some("brasa"));
        assert_eq!(config.settings.bar, BarStyle::Block);
    }

    #[test]
    fn rejects_bad_slug() {
        assert!(Config::parse("[[repo]]\nslug = \"nope\"\n").is_err());
        assert!(Config::parse("[[repo]]\nslug = \"a/\"\n").is_err());
    }

    #[test]
    fn rejects_empty_repo_list() {
        assert!(Config::parse("[settings]\nrefresh_secs = 60\n").is_err());
    }

    #[test]
    fn groups_add_their_members() {
        let config = Config::parse(
            "[[group]]\nname = \"brabo\"\nrepos = [\"o/brabo\", \"o/brabo-api\"]\n\
             [[repo]]\nslug = \"o/brabo-api\"\nlabel = \"api\"\n",
        )
        .unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].display_name(), "api");
        assert_eq!(config.group_of("o/brabo").unwrap().name, "brabo");
        assert!(config.group_of("o/other").is_none());
    }

    #[test]
    fn rejects_repo_in_two_groups() {
        let text = "[[group]]\nname = \"a\"\nrepos = [\"o/x\"]\n\
                    [[group]]\nname = \"b\"\nrepos = [\"o/x\"]\n";
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("[[repo]]\nslug = \"a/b\"\ntypo = 1\n").is_err());
    }
}
