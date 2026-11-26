# Repo by repo: unfold a repo, a workflow, a job, and read a step's log
# without leaving the terminal.
# Needs docs/demo/repos.toml (public repos only, no groups).
Output docs/assets/demo-repo.gif
Output docs/assets/demo-repo.webm
Source docs/demo/common.tape

Hide
Type "clear && popier -c docs/demo/repos.toml" Enter
Sleep 5s
Show

Sleep 2s
# Search: only what matches "main" (here, the branch) stays.
Type "/"
Sleep 400ms
Type "main"
Sleep 500ms
Enter
Sleep 2s
# Shift+Tab from the top wraps to the last repo (popier itself: calm, stable runs).
Shift+Tab
Sleep 1.2s
# Fold and unfold the repo.
Enter
Sleep 900ms
Enter
Sleep 1.2s
# Unfold a workflow: its jobs and their steps.
Down
Sleep 500ms
Enter
Sleep 2.5s
# Down to the job, then to its "Test" step (step 7), and open its log inline.
Down
Sleep 400ms
Down
Sleep 250ms
Down
Sleep 250ms
Down
Sleep 250ms
Down
Sleep 250ms
Down
Sleep 250ms
Down
Sleep 250ms
Down
Sleep 800ms
Enter
Sleep 2s
# Log lines are rows: a page down and back up scrolls the inline log into view.
PageDown
Sleep 300ms
PageUp
Sleep 2.5s
# The full log panel.
Type "l"
Sleep 2.5s
Type "G"
Sleep 1s
Type "l"
Sleep 800ms
# Esc clears the filter.
Escape
Sleep 1.5s
Hide
Type "q"
