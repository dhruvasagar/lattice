;; A CPU-bound lifecycle component: `activate` runs a bounded integer loop
;; (near-native, JIT-compiled by Cranelift) that consumes real fuel + wall
;; time but stays well inside a generous budget. Used to prove two plugins
;; run their work on two cores in parallel. `deactivate` is a no-op.
(component
	(core module $m
		(func (export "activate")
			(local $i i64)
			(loop $l
				(local.set $i (i64.add (local.get $i) (i64.const 1)))
				(br_if $l (i64.lt_u (local.get $i) (i64.const 100000000)))))
		(func (export "deactivate")))
	(core instance $inst (instantiate $m))
	(func (export "activate") (canon lift (core func $inst "activate")))
	(func (export "deactivate") (canon lift (core func $inst "deactivate")))
)
