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
| NC.2 | `scope` on `Event::BackgroundTaskFinished` + a `Stopped` outcome; the notification lays out `<icon> <scope> · <text>`; magit passes the repository name | ✅ |
| NC.3 | Pick the line that matters: failures prefer `error:` / `fatal:` / `!` / `CONFLICT` and fall back to stdout; push / fetch / pull get their own success summaries | ✅ |
| NC.4 | magit label rewrite — a human phrase naming what was acted on, no raw flags, no shared labels; partial operations report `Stopped`; the `…ing` echo bug. Landed in parts: NC.4a remote/sequencer/commit ops ✅, NC.4b one-shot and computed ops ✅, NC.4c mode helpers ✅, NC.4d `Stopped` | 🚧 |
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

## NC.2 — scope and the outcome wording ✅

- `Event::BackgroundTaskFinished { scope }` and `TaskOutcome::Stopped`.
  **No plugin-boundary change:** the event is not mirrored in WIT yet,
  and `boundary_event.rs` refuses it with `{ .. }` patterns. The plan
  had budgeted a mirror update that turned out to be unnecessary.
- `Notification::scope`, `NotificationStore::post_scoped`, and
  `task_notification(label, outcome)` as the single wording table.
- `finish_task(workdir, label, result)`: the workdir is required, not
  optional, for the same reason `finish_task` fuses its log and its
  publish. `task_scope` qualifies a basename once two checkouts share
  it.
- magit's `TaskResult` enum is introduced with `Done` / `Failed`;
  `Stopped` arrives in NC.4 with its first producer, rather than as a
  variant nothing constructs yet.

Tests: `lattice-notify` (wording per outcome, empty summary, scope kept
separate and shown in the buffer); `lattice-magit` `task_scope_tests`;
`lattice-ui-tui` bold scope span; the host test
`a_task_event_carries_its_scope_to_the_notification`, which goes
through the real subscriber.

## NC.3 — the line that matters ✅

- `lattice-magit::git_report`: `success_report(argv, stdout, stderr)` and
  `failure_report(stdout, stderr, status)`. **Reorders, never
  truncates:** the chosen line goes first and git's complete output
  follows, so `finish_task` publishes line one and still logs everything.
- Failures look for, in order: `CONFLICT`, `! [`, `fatal:`, `error:`,
  then the first line that is not `To`/`From`. stdout is searched too.
  `hint:` and progress lines are skipped. A silent failure names its
  exit status instead of printing nothing.
- Successes: push → the ref (`main`, `feature → review/feature`,
  `(forced)`, `already up to date`); fetch → `updated origin/main` /
  `N refs updated, M pruned` / `up to date`; pull → `fast-forwarded
  a..b, <stat>` / `already up to date` / the rebase line; checkout →
  `Switched to …` from stderr; stash → the `Dropped` / `Saved` line;
  clone → empty (the label names the destination).
- `resolve_upstream` now runs its own stdout-only query. It had parsed
  `run_remote_op`'s output as a value, which the report shape would
  have broken.

Tests: `git_report` unit tests for every case above; a real-git merge
conflict and a real upstream resolution (`run_remote_op_reports`).

