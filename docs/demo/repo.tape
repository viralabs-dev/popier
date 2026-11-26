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
Enter
Sleep 1.5s
Down
Sleep 500ms
Enter
Sleep 2s
Down
Sleep 500ms
Enter
Sleep 1.5s
Down
Sleep 500ms
Down
Sleep 500ms
Enter
Sleep 2.5s
Type "l"
Sleep 3s
Escape
Sleep 800ms
Type "/"
Sleep 400ms
Type "main"
Sleep 1.5s
Escape
Sleep 1s
Tab
Sleep 800ms
Tab
Sleep 1.5s
Hide
Type "q"
