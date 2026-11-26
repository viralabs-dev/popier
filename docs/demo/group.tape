# A project as one row: health bar, reasons, calm repos folded together,
# details sidebar and themes.
# Needs docs/demo/group.toml (public repos only, in one [[group]]).
Output docs/assets/demo-group.gif
Output docs/assets/demo-group.webm
Source docs/demo/common.tape

Hide
Type "clear && popier -c docs/demo/group.toml" Enter
Sleep 5s
# expand = "attention" opens a failing group; fold it so the demo can unfold it.
Enter
Sleep 500ms
Show

Sleep 2.5s
Enter
Sleep 3.5s
Enter
Sleep 1s
Type "d"
Sleep 3s
Type "d"
Sleep 600ms
Down
Sleep 600ms
Type "d"
Sleep 2s
Type "d"
Sleep 600ms
Up
Sleep 600ms
Type "t"
Sleep 1.5s
Type "t"
Sleep 1.5s
Type "t"
Sleep 1.5s
Type "t"
Sleep 1.5s
Type "t"
Sleep 1s
Hide
Type "q"
