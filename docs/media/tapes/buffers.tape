# Regenerate: vhs docs/media/tapes/buffers.tape
# Shows: everything is a buffer (differentiator 2) -- the file tree, project
# search results, and a terminal are all buffers, navigated and split with
# the same grammar as a document. Confirmed: :Tree, :search, :terminal,
# :vsplit, :ls (crates/lattice-host/src/excommand.rs).
Output assets/media/demos/buffers.gif

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
Show

Sleep 500ms
Type "lattice Cargo.toml"
Enter
Sleep 2s

# The file tree is a buffer, opened in a split -- not a fixed sidebar panel.
# `:Tree .` rather than bare `:Tree` -- the no-arg form's default-path
# resolution errors ("No such file or directory") in the preview build this
# tape was verified against; the explicit path is also what the docs show.
# `G` (last entry) rather than a fixed `jjj` -- the tree's top entries are
# directories, and a fixed motion risks landing on one and expanding it
# (verified: it lands on `.git` and reveals its internals, which is not the
# shot) instead of opening a file. `G` reliably lands on the last visible
# entry, a real file.
Type ":Tree ."
Enter
Sleep 1500ms
Type "G"
Sleep 800ms
Enter
Sleep 1500ms

# Project-wide search -- results land in a real (multi)buffer.
Type ":search TODO"
Enter
Sleep 2500ms

# A terminal, split alongside -- also just a buffer.
Type ":vsplit"
Enter
Sleep 500ms
Type ":terminal"
Enter
Sleep 1500ms
Type "echo 'a terminal is a buffer too'"
Enter
Sleep 1500ms

# Every one of the above is a listed buffer, uniformly. One Escape first --
# `:terminal` leaves the new pane in Terminal-Insert, where a typed `:`
# reaches the shell, not the ex-command line; Escape drops to
# Terminal-Normal, from which `:` opens the real command line (verified).
Escape
Sleep 500ms
Type ":ls"
Enter
Sleep 2500ms

Escape
Sleep 500ms
Type ":qa!"
Enter
Sleep 1s
