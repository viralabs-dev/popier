# popier init --discover: from nothing to a live dashboard.
# Run from the repo root: vhs docs/demo/setup.tape
# Uses a throwaway config dir so your real config is never touched, and
# --public so only public repositories show up on screen.
Output docs/assets/demo-setup.gif
Output docs/assets/demo-setup.webm
Source docs/demo/common.tape

Hide
# COLORTERM: ttyd does not set it, and without it popier falls back to the 16-color theme.
Type "export COLORTERM=truecolor POPIER_DEMO=$(mktemp -d) && cd $POPIER_DEMO && clear" Enter
Sleep 300ms
Show

Type "popier init --discover --owner viralabs-dev --public -c popier.toml"
Sleep 600ms
Enter
Sleep 4s
Hide
# Off camera: no desktop notifications while recording; the printf erases this command's line.
Type "sed -i 's/^notify = true/notify = false/' popier.toml; printf '\e[1A\e[2K'" Enter
Sleep 200ms
Show
Type "sed -n '/^\[\[group/,/^]/p' popier.toml"
Sleep 400ms
Enter
Sleep 3.5s
Type "popier -c popier.toml"
Sleep 400ms
Enter
Sleep 5s
# Fold the project into one row: health bar and reasons.
Enter
Sleep 2.5s
Down
Sleep 700ms
Down
Sleep 1.5s
Hide
Type "q"
