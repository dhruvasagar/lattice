//! LL.9: an operator bound from outside `lattice-host` (the seam `ask` uses).
//!
//! Driven through `test_helpers::press_chars`, the REAL keystroke path, because
//! `Editor::dispatch_chord` does not compose operator + motion (the absorb
//! effect pushes the prefix into `Editor::partial_chord`, a different vector
//! from the one `dispatch_chord` resolves against, so `d<motion>` fires the bare
//! motion and the test measures nothing). See `motion_composition.rs`.
//!
//! The probe operator lives here, in the test, and never in the host: that it
//! can be registered and bound with no host change is the point of the slice.
//! It does not mutate, so `OperatorSpec::mutates = false` is the only thing
//! standing between it and the read-only refusal.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::app::App;
    use crate::app::test_helpers::*;
    use lattice_grammar::SourceLocation;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::{OperatorContext, OperatorSpec};
    use lattice_keymap::KeymapLayer;
    use lattice_mode::modes::ReadOnlyMode;
    use lattice_mode::{LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
    use lattice_protocol::chord::KeyChord;

    type Spans = Arc<Mutex<Vec<((u32, u32), (u32, u32))>>>;

    const TEXT: &str = "alpha beta gamma\ndelta epsilon\n";

    struct ProbeMode;

    impl ProbeMode {
        fn id() -> ModeId {
            ModeId::new("ll9-probe-mode")
        }
    }

    impl Mode for ProbeMode {
        type Guard = ();
        fn id(&self) -> ModeId {
            Self::id()
        }
        fn kind(&self) -> ModeKind {
            ModeKind::Minor
        }
        fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn body(a: &App) -> String {
        a.editor.document.snapshot().buffer.as_string()
    }

    /// An app with `gX` bound to a non-mutating probe operator in the probe
    /// minor's layer. The probe mode is registered but NOT yet active.
    fn app_with_probe(mutates: bool) -> (App, Spans) {
        let a = app_with(TEXT, 10);
        let spans: Spans = Arc::default();
        let recorded = Arc::clone(&spans);
        let op = {
            let mut reg = (**a.editor.registry.load()).clone();
            let op = reg.register_operator(
                "operator:ll9-probe",
                "LL.9 probe: records the span it was handed and edits nothing.",
                OperatorSpec {
                    repeatable: false,
                    mutates,
                    args_schema: vec![],
                    blockwise_per_row: false,
                    post_motion_char: false,
                    apply: Arc::new(move |ctx: &mut OperatorContext| {
                        recorded.lock().unwrap().push((
                            (ctx.range.start.line, ctx.range.start.byte),
                            (ctx.range.end.line, ctx.range.end.byte),
                        ));
                        Ok(Effect::None)
                    }),
                },
            );
            a.editor.registry.store(Arc::new(reg));
            op
        };
        let mut modes = (**a.editor.mode_registry.load()).clone();
        modes.register(ProbeMode).expect("register probe mode");
        a.editor.mode_registry.store(Arc::new(modes));

        let binder = a
            .editor
            .services
            .get::<lattice_mode::OperatorChordWirerHandle>()
            .expect("the host publishes its operator binder at boot");
        binder.bind_in(
            KeymapLayer::MinorMode(ProbeMode::id()),
            SourceLocation::synthetic("ll9-test"),
            &lattice_protocol::parse_chord_sequence("gX").unwrap(),
            op,
            Some(KeyChord::char('X')),
            false,
        );
        (a, spans)
    }

    fn activate_probe(a: &mut App) {
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(&mut a.editor, buffer, ProbeMode::id());
    }

    fn take(spans: &Spans) -> Vec<((u32, u32), (u32, u32))> {
        std::mem::take(&mut *spans.lock().unwrap())
    }

    fn esc(a: &mut App) {
        press(
            a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
    }

    #[test]
    fn it_composes_with_motions_text_objects_counts_and_doubling() {
        let (mut a, spans) = app_with_probe(false);
        activate_probe(&mut a);

        press_chars(&mut a, "gXw");
        assert_eq!(take(&spans), vec![((0, 0), (0, 6))], "motion `w`");
        press_chars(&mut a, "gXiw");
        assert_eq!(take(&spans), vec![((0, 0), (0, 5))], "text object `iw`");
        press_chars(&mut a, "gX2w");
        assert_eq!(
            take(&spans),
            vec![((0, 0), (0, 11))],
            "count after the chord"
        );
        press_chars(&mut a, "2gXw");
        assert_eq!(
            take(&spans),
            vec![((0, 0), (0, 11))],
            "count before the chord"
        );
        press_chars(&mut a, "gXX");
        let doubled = take(&spans);
        assert_eq!(doubled.len(), 1, "doubled form runs once: {doubled:?}");
        assert_eq!(doubled[0].0, (0, 0), "doubled form starts at the line");
        assert_eq!(body(&a), TEXT, "the probe edits nothing");
    }

    #[test]
    fn it_composes_with_visual() {
        let (mut a, spans) = app_with_probe(false);
        activate_probe(&mut a);
        press_chars(&mut a, "vlgX");
        let got = take(&spans);
        assert_eq!(got.len(), 1, "Visual `gX` runs once: {got:?}");
        assert_eq!(got[0].0, (0, 0));
        assert!(got[0].1.1 >= 1, "covers the selection: {got:?}");
    }

    #[test]
    fn it_lives_only_where_its_minor_is_active() {
        let (mut a, spans) = app_with_probe(false);
        press_chars(&mut a, "gXw");
        esc(&mut a);
        assert!(
            take(&spans).is_empty(),
            "unbound while the minor is inactive"
        );

        activate_probe(&mut a);
        press_chars(&mut a, "gXw");
        assert_eq!(take(&spans).len(), 1, "bound once the minor is active");

        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::deactivate_minor_by_id(&mut a.editor, buffer, ProbeMode::id());
        esc(&mut a);
        press_chars(&mut a, "gXw");
        assert!(take(&spans).is_empty(), "gone again once it is deactivated");
    }

    fn make_read_only(a: &mut App) {
        let buffer = a.editor.document_buffer_id;
        lattice_mode::ModeActivator::activate_minor_by_id(
            &mut a.editor,
            buffer,
            ReadOnlyMode::mode_id(),
        );
    }

    #[test]
    fn a_non_mutating_operator_runs_on_a_read_only_buffer() {
        let (mut a, spans) = app_with_probe(false);
        activate_probe(&mut a);
        make_read_only(&mut a);
        press_chars(&mut a, "gXw");
        assert_eq!(take(&spans), vec![((0, 0), (0, 6))]);
        assert_eq!(body(&a), TEXT);
    }

    #[test]
    fn an_operator_that_mutates_is_refused_on_a_read_only_buffer() {
        // The same probe, declared mutating: the flag alone decides.
        let (mut a, spans) = app_with_probe(true);
        activate_probe(&mut a);
        press_chars(&mut a, "gXw");
        assert_eq!(
            take(&spans).len(),
            1,
            "control: it runs on a writable buffer"
        );

        make_read_only(&mut a);
        press_chars(&mut a, "gXw");
        assert!(
            take(&spans).is_empty(),
            "refused once the buffer is read-only"
        );
        let msg = a
            .editor
            .last_message
            .as_ref()
            .map(|m| m.text.to_lowercase());
        assert!(msg.is_some_and(|m| m.contains("read-only")), "and says why");
    }

    #[test]
    fn yank_still_works_and_delete_and_change_still_refuse_on_a_read_only_buffer() {
        let mut a = app_with(TEXT, 10);
        make_read_only(&mut a);

        press_chars(&mut a, "yw");
        assert_eq!(
            a.editor
                .unnamed_register
                .as_ref()
                .map(|r| r.content.as_str()),
            Some("alpha "),
            "`yw` reads"
        );
        assert_eq!(body(&a), TEXT);

        press_chars(&mut a, "dw");
        assert_eq!(body(&a), TEXT, "`dw` is refused");
        press_chars(&mut a, "cw");
        assert_eq!(body(&a), TEXT, "`cw` is refused");
    }
}
