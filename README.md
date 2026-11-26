# popier

Watch the GitHub Actions pipelines of many repositories on one screen, with
every **workflow → job → step** laid out as a tree.

```
▾ acme/api
  ▾ ✗ CI (main)
        ✗ test
            ✓ 1. Set up job
            ✓ 2. Checkout
            ✗ 3. Run tests
  ▸ ✓ Release (main)
▾ acme/web
  ▾ ● Deploy (main)
        ● deploy
            ✓ 1. Set up job
            ● 2. Build
            ○ 3. Upload
```

`✓` success · `●` running · `○` queued · `✗` failed · `–` skipped · `⊘` cancelled

## Install

**Prebuilt binary (Linux x86_64):** download `popier-x86_64-unknown-linux-musl.tar.gz`
(static, any distro) or `popier-x86_64-unknown-linux-gnu.tar.gz` from the
[latest release](https://github.com/viralabs-dev/popier/releases/latest):

```sh
curl -sSL https://github.com/viralabs-dev/popier/releases/latest/download/popier-x86_64-unknown-linux-musl.tar.gz | tar xz
install -m 755 popier ~/.local/bin/
```

**From source** (Rust 1.85+):

```sh
cargo install --git https://github.com/viralabs-dev/popier
```

Notifications use `notify-send` (libnotify), available on most Linux desktops.

## Setup

```sh
popier init          # writes ~/.config/popier/config.toml
$EDITOR ~/.config/popier/config.toml
```

The config lists repositories in display order:

```toml
[settings]
refresh_secs = 60        # seconds between refreshes
expand = "attention"     # "attention" (running/queued/failed) or "all"
runs_per_repo = 30       # recent runs scanned per repo
notify = true            # desktop notification when a workflow fails / recovers
notify_branches = []     # only notify for these branches (empty = all)

[[repo]]
slug = "your-org/your-repo"

[[repo]]
slug = "your-org/another-repo"
label = "another"        # optional display name
branch = "main"          # optional branch filter
```

See [`examples/config.toml`](examples/config.toml) for the full sample.

### Authentication

popier talks to the GitHub REST API directly. The token is taken from, in
order: `GITHUB_TOKEN`, `GH_TOKEN`, then `gh auth token`. If you already use the
`gh` CLI, nothing else is needed.

## Usage

```sh
popier               # interactive dashboard
popier once          # print the tree once and exit
popier once --all    # expand every workflow
popier -c other.toml # use another config file
```

### Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move |
| `Enter` / `Space` | fold or unfold a repo or workflow (unfolding fetches its jobs) |
| `Tab` / `Shift+Tab` | next / previous repo |
| `/` | filter by part of a branch name, or `#152` for a PR (applies as you type; `Esc` clears) |
| `l` | show the log of the selected step (or job) in a panel; inside it `w` toggles step/whole job, `Esc` closes |
| `a` | toggle expanding every workflow |
| `r` | refresh now |
| `o` | open the selected item in the browser |
| `q` / `Esc` | quit (`Esc` clears an active filter first) |

## Filtering

`/` opens a filter field in the footer. Text matches part of the head branch
(case-insensitive); `#152` matches runs of pull request 152. The filter applies
to the recent runs already scanned per repo (`runs_per_repo`), so a branch with
no run among them shows as "(no runs)". Notifications pause while a filter is
on. For a permanent per-repo branch filter, use `branch` in the config.

## Step logs

`l` on a step opens a panel with the last 200 lines of that step; `w` switches
to the whole job log, `Esc` closes. GitHub serves logs per job only, so popier
downloads the job log once (kept in memory for the session) and cuts the step
at its `##[group]Run …` marker, up to the next step's marker (falling back to
the step's start and end times when it has none). If the step can't be found, the end of the job log
is shown with a note. The download follows GitHub's redirect to a signed
storage URL without sending your token there.

## Notifications

While the dashboard is open, popier sends a desktop notification
(`notify-send`) when a workflow that was in any other state starts failing,
and a low-urgency one when a failing workflow passes again. Nothing is sent
for failures that were already there when popier started, and each run is
announced at most once. Turn it off with `notify = false`, or limit it with
`notify_branches = ["main", "dev"]`.

## API usage

Each refresh makes two requests per repository (recent runs and workflow names)
plus one per expanded workflow.
Responses are cached with their ETag, so unchanged data comes back as
`304 Not Modified`, which does not count against the 5,000 requests/hour limit.
The footer shows the remaining quota.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Architecture notes live in [`docs/architecture.md`](docs/architecture.md).

## Releasing

Push a tag `vX.Y.Z` matching the version in `Cargo.toml`; the release workflow
builds the Linux binaries (glibc and static musl) and attaches them to a GitHub release.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Contributions are accepted under
the same terms.
