;; A runaway lifecycle component: `activate` is an infinite loop. Under a small
;; fuel budget it traps cleanly with `TrapKind::Fuel` — the runaway-guard the
;; host relies on so a looping plugin can never freeze the editor.
;; `deactivate` is a no-op (so teardown doesn't spin).
(component
	(core module $m
		(func (export "activate")
			(loop $l (br $l)))
		(func (export "deactivate")))
	(core instance $inst (instantiate $m))
	(func (export "activate") (canon lift (core func $inst "activate")))
	(func (export "deactivate") (canon lift (core func $inst "deactivate")))
)
