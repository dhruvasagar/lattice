# Regenerate: vhs docs/media/tapes/magit.tape
# Shows: a real magit port inside a modal editor (differentiator 1) --
# status, an untracked file, and staging it with `s` (evil-magit convention,
# confirmed docs/user/magit-diff-mode.md:24 / magit-hunk-mode.md:37).
#
# Self-contained: creates its own throwaway untracked file so the demo has
# something to show and stage regardless of what else is dirty in the
# ambient working tree, finds it with `/` (a fixed motion like `jjj` is NOT
# reliable here -- the section it lands in depends on how many OTHER files
# happen to be dirty when this renders, verified against the real repo
# state while writing this tape), and reverts it afterward.
Output assets/media/demos/magit.gif

Require lattice

Set Shell "bash"
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 20
Set TypingSpeed 60ms

Hide
Type "cd /Users/dhruva/src/dhruvasagar/lattice && clear"
Enter
# A new, UNTRACKED throwaway file so magit-status has a real entry to show
# and stage, with zero risk to any existing content -- `rm` cleans it up
# below, no matter what state it's in (staged or not) when the tape ends.
Type "echo '# magit demo scratch file' > docs/media/.magit-demo-scratch.md"
Enter
Show

Sleep 500ms
Type "lattice"
Enter
Sleep 3s

Type ":magit-status"
Enter
Sleep 2500ms

# Find the demo entry and stage it -- the thing no other modal editor does.
Type "/magit-demo-scratch"
Enter
Sleep 1s
Type "s"
Sleep 2s

Escape
Sleep 500ms
Type ":q!"
Enter
Sleep 1s

Hide
Type "clear"
Enter
# Remove the demo scratch file and unstage it if `s` staged it -- covers
# both landing spots, leaves the tree exactly as it was before this tape.
Type "cd /Users/dhruva/src/dhruvasagar/lattice && git reset -q -- docs/media/.magit-demo-scratch.md; rm -f docs/media/.magit-demo-scratch.md"
Enter
Sleep 500ms
