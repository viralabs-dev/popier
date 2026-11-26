# Architecture

```
config.toml ─▶ Config ─▶ snapshot::take ─▶ Snapshot ─▶ tree::build ─▶ Vec<Row>
                              │                                         │
                        github::Client                         ┌────────┴────────┐
                     (REST + ETag cache)                       tui (ratatui)   render_plain
```

| Module | Responsibility |
|---|---|
| `config` | Parse and validate `config.toml`. |
| `github` | Thin REST client for `actions/runs`, `actions/workflows` and `runs/{id}/jobs`; ETag cache; token discovery. |
| `status` | Maps GitHub `status` + `conclusion` to one `Status` with icon and color. |
| `snapshot` | Fetches every repo concurrently, keeps the latest run per workflow, fetches jobs/steps for expanded workflows. |
| `tree` | Flattens a snapshot into display rows, honouring user folds. Shared by both front ends. |
| `filter` | Parses the `/` filter (branch part or `#PR`) and matches runs before the latest run per workflow is picked. |
| `logs` | Cuts a step out of a job log by timestamps; strips timestamps; tails. |
| `notify` | Compares consecutive snapshots and sends `notify-send` when a workflow starts failing or recovers. |
| `tui` | Event loop. A background task produces snapshots on a timer or on demand; the UI thread only draws and handles keys. |

## Decisions

- **Rust + ratatui TUI.** Single static binary, no runtime to install, fast redraws.
- **GitHub REST API instead of shelling out to `gh`.** Enables ETag conditional
  requests (cheap polling) and concurrent requests without spawning processes.
  `gh` is still used, optionally, to obtain a token.
- **Config file, not code.** Any set of repositories, in any order, with optional
  branch filter and label.
- **Expansion rule.** Running, queued and failed workflows open automatically;
  others open on demand (`Enter`) or with `a`. This keeps API usage proportional
  to what needs attention.

## Not yet decided / next steps

- Whether the project becomes public (license, packaging).
