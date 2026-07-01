;; The degenerate `init.rs`: a hand-written no-op component implementing the
;; `plugin` lifecycle world (../../../../wit/plugin.wit). Its `activate` /
;; `deactivate` exports do nothing. Assembled to component bytes by the `wat`
;; crate inside the tests + the instantiation bench -- no wasm32-wasip2 target
;; or separate guest build for the PH7.0 scaffold (that arrives with the real
;; Rust guest at PH7.4).
(component
	;; A core module with two empty functions.
	(core module $noop
		(func (export "activate"))
		(func (export "deactivate"))
	)
	(core instance $inst (instantiate $noop))
	;; Lift the core funcs into component-level funcs whose export names match
	;; the `plugin` world's `activate` / `deactivate`.
	(func (export "activate") (canon lift (core func $inst "activate")))
	(func (export "deactivate") (canon lift (core func $inst "deactivate")))
)
