# popier init --discover: from nothing to a live dashboard.
# Run from the repo root: vhs docs/demo/setup.tape
# Uses a throwaway config dir so your real config is never touched, and
# --public so only public repositories show up on screen.
Output docs/assets/demo-setup.gif
Output docs/assets/demo-setup.webm
Source docs/demo/common.tape

Hide
Type "export POPIER_DEMO=$(mktemp -d) && cd $POPIER_DEMO && clear" Enter
Sleep 300ms
Show

Type "popier init --discover --owner viralabs-dev --public -c popier.toml"
Sleep 600ms
Enter
Sleep 4s
Type "sed -i 's/^notify = true/notify = false/' popier.toml && head -n 24 popier.toml"
Sleep 400ms
Enter
Sleep 4s
Type "popier -c popier.toml"
Sleep 400ms
Enter
Sleep 5s
Down
Sleep 700ms
Down
Sleep 700ms
Down
Sleep 1.5s
Type "q"
Sleep 500ms
