# popier

Watch the GitHub Actions pipelines of many repositories on one screen, with
every **workflow → job → step** laid out as a tree.

```
▾ viralabs-dev/brabo-api
  ▾ ✗ CI (main)
        ✗ test
            ✓ 1. Set up job
            ✓ 2. Checkout
            ✗ 3. Run tests
  ▸ ✓ Release (main)
▾ viralabs-dev/brabo-web
  ▾ ● Deploy (main)
        ● deploy
            ✓ 1. Set up job
            ● 2. Build
            ○ 3. Upload
```

`✓` success · `●` running · `○` queued · `✗` failed · `–` skipped · `⊘` cancelled

## Install

Requires a Rust toolchain (`rustup`).

```sh
cargo install --path .
```

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

[[repo]]
slug = "viralabs-dev/brabo"

[[repo]]
slug = "viralabs-dev/brabo-api"
label = "api"            # optional display name
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
| `a` | toggle expanding every workflow |
| `r` | refresh now |
| `o` | open the selected item in the browser |
| `q` / `Esc` | quit |

## API usage

Each refresh makes one request per repository plus one per expanded workflow.
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
