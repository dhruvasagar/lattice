# Slice plan — notification clarity: icons, scope, and saying what happened

Design: [`../../architecture/notifications.md`](../../architecture/notifications.md)
(§"Icons and the success level", §"Scope, and saying what happened").

Built on NOTIF.1a–f. The reported gap had two halves:

- **Icons.** Rows were colour-coded by level but carried no icon, and a
  finished operation looked the same as any neutral note because "success"
  was just Info.
- **Clarity.** An audit of magit's ~100 background-task producers found
  that none named the repository, ~40 labels were raw git syntax
  (`cherry-pick --continue`, `rebase onto @{push}`, a whole argv with a
  sha), ~45 succeeded with empty output ("X finished"), ~12 labels were
  shared by different operations, and the one-line summary was often the
  wrong line (`push: To <url>`; `merge failed: ` with nothing after it,
  because git writes `CONFLICT` to stdout). Several at once could not be
  told apart.

| Slice | What | Status |
|---|---|---|
| NC.1 | `NotificationLevel::Success`; one shared `glyph(nerd_fonts)` for the TUI, GPUI and the `*notifications*` buffer; theme-sourced colours in both peers; the buffer re-renders on a `ui.nerd_fonts` flip | ✅ |
| NC.2 | `scope` on `Event::BackgroundTaskFinished` + a `Stopped` outcome; plugin-boundary mirror; the notification lays out `<icon> <scope> · <text>`; magit passes the repository name | 📝 |
| NC.3 | Pick the line that matters: failures prefer `error:` / `fatal:` / `!` / `CONFLICT` and fall back to stdout; push / fetch / pull get their own success summaries | 📝 |
| NC.4 | magit label rewrite — a human phrase naming what was acted on, no raw flags, no shared labels; partial operations report `Stopped`; the `…ing` echo bug | 📝 |
| NC.5 | Report the eight actions that finish silently (file stage/unstage/discard, branch create/checkout/rename/delete) | 📝 |

## NC.1 — icons and the success level ✅

- `NotificationLevel::Success` is Info in timeout and in the `*messages*`
  tee; only its icon and colour differ.
- `NotificationLevel::glyph(nerd_fonts)` is the one place the icons are
  chosen. Nerd palette: `nf-fa-circle_info / circle_check /
  triangle_exclamation / circle_xmark`. Fallback: `● ✓ ▲ ✗`, the same
  shapes the diagnostic gutter falls back to.
- Colours: `diagnostic.{info,warning,error}` and `diff.add.sign`, read by
  both peers. The TUI previously used fixed ANSI colours, and GPUI tinted
  info with `cursor_background`.
- The `*notifications*` buffer re-renders in place on an `OptionChanged`
  for `ui.nerd_fonts`, without taking focus.

Tests: `lattice-notify` unit tests (distinct icons per palette, one char
each, fallback outside the Private Use Area, Success timing = Info);
`lattice-ui-tui` `notification_line_tests`;
`lattice-host/tests/notifications_follow_the_icon_palette.rs` (fails with
the refresh removed).
