# Regenerate: vhs docs/media/tapes/config.tape
# Shows: config is Rust compiled to WASM, and it is PROGRAMMATIC -- a real
# ex-command defined in init.rs, not a remapped key (differentiator 3).
# Confirmed: `lattice --scaffold-init` (crates/lattice-cli), `:reload-config`
# (crates/lattice-host/src/excommand.rs), the `grammar-plugin` world and
# `register_ex_command` (docs/user/init.md "Custom grammar" section, verified
# against wit/grammar.wit -- fixtures/config-demo-init.rs is that verified
# source, confirmed to build against this editor's WIT before this tape was
# written).
#
# Self-contained: scaffolds a fresh init under its own throwaway $HOME, drops
# in the fixture source (a real custom :hello command), and builds it --
# never touches a real user's ~/.config/lattice. The `cargo build` runs
# Hidden -- it's the real workflow, not a special-cased skip.
#
# No quoted variable expansions below: `mktemp -d` never produces a path
# with spaces on macOS/Linux, and VHS's tape lexer does not accept an
# escaped `\"` inside a `Type` string (tested against vhs 0.12.0).
Output assets/media/demos/config.gif

Require lattice
Require cargo

Set Shell "bash"
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 20
Set TypingSpeed 60ms

Hide
Type "export REPO=$(pwd) && export DEMO_HOME=$(mktemp -d) && clear"
Enter
Type "HOME=$DEMO_HOME lattice --scaffold-init"
Enter
Sleep 1s
Type "cp $REPO/docs/media/tapes/fixtures/config-demo-plugin.toml $DEMO_HOME/.config/lattice/init/plugin.toml"
Enter
Type "cp $REPO/docs/media/tapes/fixtures/config-demo-init.rs $DEMO_HOME/.config/lattice/init/src/lib.rs"
Enter
Type "cd $DEMO_HOME/.config/lattice/init && cargo build --release --target wasm32-wasip2 -q && cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm"
Enter
Sleep 25s
Type "clear"
Enter
Show

# Show the programmatic command being defined -- this is the shot: a real
# Rust function, not a remapped key.
Type "HOME=$DEMO_HOME lattice src/lib.rs"
Enter
Sleep 3s
Type "gg"
Sleep 500ms

Type ":reload-config"
Enter
Sleep 1500ms

Type ":hello Lattice"
Enter
Sleep 2s

Type ":q!"
Enter
Sleep 500ms

Hide
Type "rm -rf $DEMO_HOME"
Enter
Sleep 500ms
