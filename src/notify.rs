//! Desktop notifications when a workflow starts failing or recovers.
//!
//! Each new snapshot is compared with the previous one. Nothing is sent for
//! the first snapshot, so opening popier never floods the desktop with
//! failures that were already there.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use crate::snapshot::Snapshot;
use crate::status::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// Was anything but failed, now failed.
    Failed,
    /// Was failed, now succeeded.
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub change: Change,
    pub repo: String,
    pub workflow: String,
    pub branch: Option<String>,
    pub run_id: u64,
}

/// Remembers the last status of each workflow and which runs were announced.
#[derive(Debug, Default)]
pub struct Watcher {
    /// (repo slug, workflow id) → last seen status. `None` until the first snapshot.
    last: Option<HashMap<(String, u64), Status>>,
    announced: HashSet<(u64, Change)>,
    branches: Vec<String>,
}

impl Watcher {
    pub fn new(branches: Vec<String>) -> Self {
        Self {
            branches,
            ..Self::default()
        }
    }

    /// Forgets the previous snapshot, so the next one is silent.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Records `snapshot` and returns the changes worth announcing.
    pub fn observe(&mut self, snapshot: &Snapshot) -> Vec<Event> {
        let mut current = HashMap::new();
        let mut events = Vec::new();

        for repo in &snapshot.repos {
            let Ok(workflows) = &repo.workflows else {
                // Keep previous statuses for a repo that failed to load this time.
                if let Some(last) = &self.last {
                    for (key, status) in last.iter().filter(|((slug, _), _)| *slug == repo.slug) {
                        current.insert(key.clone(), *status);
                    }
                }
                continue;
            };
            for wf in workflows {
                let key = (repo.slug.clone(), wf.workflow_id);
                let previous = self.last.as_ref().and_then(|last| last.get(&key)).copied();
                current.insert(key, wf.status);

                let change = match (previous, wf.status) {
                    (Some(prev), Status::Failure) if prev != Status::Failure => Change::Failed,
                    (Some(Status::Failure), Status::Success) => Change::Recovered,
                    _ => continue,
                };
                if !self.branch_allowed(wf.branch.as_deref()) {
                    continue;
                }
                if !self.announced.insert((wf.run_id, change)) {
                    continue;
                }
                events.push(Event {
                    change,
                    repo: repo.name.clone(),
                    workflow: wf.name.clone(),
                    branch: wf.branch.clone(),
                    run_id: wf.run_id,
                });
            }
        }

        self.last = Some(current);
        events
    }

    fn branch_allowed(&self, branch: Option<&str>) -> bool {
        self.branches.is_empty() || branch.is_some_and(|b| self.branches.iter().any(|x| x == b))
    }
}

/// Sends `event` as a desktop notification: `notify-send` on Linux, a toast
/// through Windows PowerShell on Windows. Failures are ignored on purpose:
/// a missing notification daemon must never break the dashboard.
pub fn send(event: &Event) {
    let verb = match event.change {
        Change::Failed => "failed",
        Change::Recovered => "passing again",
    };
    let title = format!("popier: {} › {} {verb}", event.repo, event.workflow);
    let body = event.branch.as_deref().unwrap_or("-");
    let _ = command(event.change, &title, body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(not(windows))]
fn command(change: Change, title: &str, body: &str) -> Command {
    let (urgency, icon) = match change {
        Change::Failed => ("critical", "dialog-error"),
        Change::Recovered => ("low", "emblem-ok"),
    };
    let mut cmd = Command::new("notify-send");
    cmd.args(["--app-name=popier", "-u", urgency, "-i", icon, title, body]);
    cmd
}

/// Shows a toast through the WinRT API, which only Windows PowerShell 5.1
/// (`powershell.exe`, present on every Windows) can load; `pwsh` cannot.
/// Title and body travel in environment variables and are XML-escaped by the
/// script, so repo or branch names can't inject PowerShell or toast markup.
/// The script has no double quotes: Windows PowerShell 5.1 drops escaped
/// ones from its command line. The app id is PowerShell's own, so no
/// registration is needed.
#[cfg(windows)]
fn command(_change: Change, title: &str, body: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "\
$m = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime];\
$x = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]::new();\
$e = [Security.SecurityElement];\
$x.LoadXml('<toast><visual><binding template=''ToastGeneric''><text>' + $e::Escape($env:POPIER_TITLE) + '</text><text>' + $e::Escape($env:POPIER_BODY) + '</text></binding></visual></toast>');\
$m::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($x))";
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("POPIER_TITLE", title)
        .env("POPIER_BODY", body)
        .creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RepoView, WorkflowView};

    fn snap(runs: &[(u64, Status, &str)]) -> Snapshot {
        Snapshot {
            repos: vec![RepoView {
                name: "api".into(),
                slug: "o/api".into(),
                group: None,
                workflows: Ok(runs
                    .iter()
                    .map(|&(run_id, status, branch)| WorkflowView {
                        workflow_id: 1,
                        run_id,
                        name: "CI".into(),
                        branch: Some(branch.into()),
                        status,
                        url: String::new(),
                        started_at: None,
                        updated_at: None,
                        jobs: None,
                    })
                    .collect()),
            }],
        }
    }

    fn changes(events: Vec<Event>) -> Vec<Change> {
        events.into_iter().map(|e| e.change).collect()
    }

    #[test]
    fn first_snapshot_is_silent() {
        let mut w = Watcher::new(vec![]);
        assert!(w.observe(&snap(&[(1, Status::Failure, "main")])).is_empty());
    }

    #[test]
    fn announces_new_failure_once() {
        let mut w = Watcher::new(vec![]);
        w.observe(&snap(&[(1, Status::Running, "main")]));
        assert_eq!(
            changes(w.observe(&snap(&[(1, Status::Failure, "main")]))),
            vec![Change::Failed]
        );
        // Still failing: nothing new.
        assert!(w.observe(&snap(&[(1, Status::Failure, "main")])).is_empty());
        // A new run that also fails keeps the workflow failing: no repeat.
        assert!(w.observe(&snap(&[(2, Status::Failure, "main")])).is_empty());
    }

    #[test]
    fn announces_recovery() {
        let mut w = Watcher::new(vec![]);
        w.observe(&snap(&[(1, Status::Failure, "main")]));
        w.observe(&snap(&[(2, Status::Running, "main")]));
        // Running → success is not a recovery; failure must be the previous state.
        assert!(w.observe(&snap(&[(2, Status::Success, "main")])).is_empty());

        let mut w = Watcher::new(vec![]);
        w.observe(&snap(&[(1, Status::Failure, "main")]));
        assert_eq!(
            changes(w.observe(&snap(&[(2, Status::Success, "main")]))),
            vec![Change::Recovered]
        );
    }

    #[test]
    fn same_run_is_not_announced_twice() {
        let mut w = Watcher::new(vec![]);
        w.observe(&snap(&[(1, Status::Running, "main")]));
        assert_eq!(w.observe(&snap(&[(1, Status::Failure, "main")])).len(), 1);
        w.observe(&snap(&[(1, Status::Running, "main")])); // re-run of the same run id
        assert!(w.observe(&snap(&[(1, Status::Failure, "main")])).is_empty());
    }

    #[test]
    fn branch_filter() {
        let mut w = Watcher::new(vec!["main".into()]);
        w.observe(&snap(&[(1, Status::Running, "feat/x")]));
        assert!(
            w.observe(&snap(&[(1, Status::Failure, "feat/x")]))
                .is_empty()
        );
        w.observe(&snap(&[(2, Status::Running, "main")]));
        assert_eq!(w.observe(&snap(&[(2, Status::Failure, "main")])).len(), 1);
    }

    #[test]
    fn repo_error_keeps_previous_state() {
        let mut w = Watcher::new(vec![]);
        w.observe(&snap(&[(1, Status::Running, "main")]));
        let mut broken = snap(&[]);
        broken.repos[0].workflows = Err("network".into());
        assert!(w.observe(&broken).is_empty());
        assert_eq!(w.observe(&snap(&[(1, Status::Failure, "main")])).len(), 1);
    }
}
