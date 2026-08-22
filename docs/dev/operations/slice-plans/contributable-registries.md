# Contributable registries — slice plan

**Design fragment:**
[`../../architecture/contributable-registries.md`](../../architecture/contributable-registries.md).

**Status:** CR.1–CR.5 📝.

Closes HD.6 (`help-docs.md`) and DB.8 (`dashboard.md`) — both were
blocked on the same missing mechanism, so they are sequenced here as one
track rather than two. The two source plans point here and stay active
until this one completes.

## Sequencing

```
CR.1 (help handle)      ─┐
CR.2 (dashboard handle) ─┴─> substrate: both registries runtime-writable

CR.3 (help WIT seam)    ─ depends on CR.1
CR.4 (dashboard WIT seam)─ depends on CR.2

CR.5 (docs + bench + ledger) ─ depends on CR.3, CR.4
```

CR.1 and CR.2 are independent of each other; CR.3 and CR.4 are
independent of each other. Neither pair needs the other's half, so a
green CR.1 can ship before CR.2 is started.

| Slice | Description | Status |
|---|---|---|
| CR.1 | `HelpTopicRegistryHandle` — topics behind `Arc`, RCU handle, Phase-A service | 📝 |
| CR.2 | `DashboardRegistryHandle` — RCU handle, last-wins resolution, shadow-restoring unload | 📝 |
| CR.3 | `help` WIT seam — `help-plugin` world, namespaced topics, drain + teardown | 📝 |
| CR.4 | `dashboard` WIT seam — `dashboard-plugin` world, `WasmDashboardSection`, drain + teardown | 📝 |
| CR.5 | User docs, bench, ledger, source-plan closeout | 📝 |

---

### CR.1 — `HelpTopicRegistryHandle` 📝

Make the help registry writable after boot. **No plugin surface in this
slice** — it is pure substrate, and it lands green on its own.

- `HelpTopicRegistry::by_name` becomes `HashMap<String, Arc<HelpTopic>>`
  so the registry derives `Clone`. The `Arc` is load-bearing beyond
  clone cost: `HelpTopicBody::Compressed` holds a `OnceLock<String>`
  decompression cache, and sharing it means an RCU write does not throw
  away every already-inflated body.
- `pub type HelpTopicRegistryHandle = Arc<ArcSwap<HelpTopicRegistry>>`
  plus `HelpTopicRegistry::new_handle()`, mirroring
  `CompilationParserFactories::new_handle` so consumers do not each name
  `arc_swap`.
- `HelpTopic` gains `plugin_id: Option<u64>` (`None` for builtins) and
  the registry gains `unregister_plugin(id) -> usize`, idempotent.
- `Editor::help_topics: Arc<HelpTopicRegistry>` →
  `HelpTopicRegistryHandle`; the five read sites (`dispatch.rs` ×4,
  `editor_boot.rs`) take a `.load()` snapshot.
- `HelpTopicsGenerator` holds the handle, not a snapshot — otherwise
  `:help <Tab>` enumerates the boot-time set forever and CR.3's topics
  are unreachable by completion.
- Registered as a Phase-A boot service so CR.3's loader drain resolves
  it (`register_service::<HelpTopicRegistryHandle>`), alongside the
  `ConfigRegistry` hoist.

*paramount:* #2 (the mechanism), #1 (reads stay wait-free snapshots).
*test:* an RCU write is visible to a handle captured beforehand; a
snapshot taken before the write still reads the old set (coherence);
`unregister_plugin` is idempotent and removes only that plugin's
topics; a shared `OnceLock` cache survives a clone (inflate once, clone,
assert the clone does not re-inflate).
*doc:* design §2; `topics.rs` module docs lose the "known limit"
paragraph.

### CR.2 — `DashboardRegistryHandle` 📝

Same treatment for the dashboard, plus the shadow-restore that
replace-by-id forces.

- `pub type DashboardRegistryHandle = Arc<ArcSwap<DashboardRegistry>>`;
  `lattice_dashboard::install` registers the handle instead of the
  plain value. Already ordered correctly (boot 667 vs the loader's
  1808).
- `DashboardSection` gains `fn plugin_id(&self) -> Option<u64> { None }`
  — a defaulted method, so no builtin section changes.
- `register` **appends** instead of overwriting; id resolution takes the
  **last** registration. A re-registration carrying the same
  `plugin_id` for the same id replaces in place, so a reload cannot grow
  the stack. `ids()` de-duplicates.
- `unregister_plugin(id)` is a `retain`; the builtin a plugin displaced
  resurfaces automatically because it was never removed.
- `Editor::compose_dashboard_sections` reads the handle.

*paramount:* #2; #1 (compose snapshots once).
*test:* a plugin section replacing `getting-started` displaces it in
`ordered()`; `unregister_plugin` restores the builtin in the same
position; two plugins shadowing one id unwind in reverse order; the
same owner re-registering the same id does not grow the stack.
*doc:* design §3.2; `dashboard.md` §3.1.

### CR.3 — the `help` WIT seam 📝

- `wit/help.wit`: the `help` interface and the `help-plugin` world (§3.1
  of the design). Async world, `theme-plugin` shape.
- `PluginSeam::Help` + its `"help"` wire name. The loader's drain
  `match` is exhaustive, so the compiler flags the missing arm rather
  than a silent skip.
- `lattice-plugin-host/src/help_host.rs`: `spawn_help_plugin` calls the
  export once, collecting `(name, summary, body, related)` into store
  state via the `register-topic` host func, and returns the collected
  specs. **The host does not depend on `lattice-help`** — it returns
  plain data and the loader builds the `HelpTopic`s, which keeps the
  dependency where it already points.
- Namespacing (`<plugin-id>.<name>`, bare id when `name` is empty or
  equals the plugin id) happens host-side, from the host's own manifest
  ground truth. A guest cannot forge a name outside its namespace.
- `PluginLoader::drain_help` RCU-registers into the handle stamped with
  the plugin id; `PluginTeardown` calls `unregister_plugin`.
- Fixture plugin under the plugin-host test fixtures, with real
  `include_str!`'d markdown, proving the embed-in-component path end to
  end.

*paramount:* #2.
*error handling:* a malformed spec (empty name, empty body) is warned
and skipped — the plugin's other topics still register; never a trap,
never a partially-registered topic.
*test:* a loaded fixture's topic is reachable by `:help <plugin>.<page>`
and by the bare `:help <plugin>`; it appears in `:help <Tab>`
completion; unload removes it and a builtin topic is untouched; a
reload does not duplicate; two plugins with the same internal topic name
do not collide.
*doc:* design §3.1; `docs/user/plugins.md`.

### CR.4 — the `dashboard` WIT seam 📝

- `wit/dashboard.wit`: the `dashboard` interface and the
  `dashboard-plugin` world (§3.2). **Sync** world on the grammar linker.
- `PluginSeam::Dashboard` + drain arm.
- `lattice-plugin-host/src/dashboard_host.rs`:
  `WasmDashboardSection { store: Mutex<Store>, bindings, id, order,
  default_enabled, plugin_id, poisoned }` implementing
  `lattice_dashboard::DashboardSection`. `render` calls
  `render-section` under `PluginBudget::grammar()`; a trap poisons the
  section and it renders an empty fragment from then on, warned once.
  Adds a `lattice-dashboard` dep to the host (leaf, no cycle).
- Instantiate-once-at-load probe, `error_parser_factory`'s argument: a
  component that cannot start fails the load loudly rather than
  contributing nothing to every compose forever.
- Guest output is untrusted: rows with no spans, spans with an
  unparseable `link-target`, and a fragment exceeding a row cap are
  dropped at `debug!`, never trapped on.
- `PluginLoader::drain_dashboard` + teardown by provenance.

*paramount:* #2; #1 (budgeted, Display-class only — named as an
accepted cost in design §3.2).
*test:* a fixture section appears in the composed page in its `order`
slot; it sees `ctx.nerd_fonts` (compose twice with the option flipped
and assert the rows differ); a trapping fixture leaves the rest of the
page intact; unload restores a displaced builtin; a fixture returning a
malformed fragment is dropped, not fatal.
*doc:* design §3.2; `dashboard.md` §6, §10 (DB.8 moves from rejected-
for-v1 to implemented).

### CR.5 — docs, bench, ledger 📝

- `docs/user/plugins.md`: both seams, with the `include_str!` pattern
  for help bodies spelled out — it is the non-obvious half.
- Bench: extend `crates/lattice-help/benches/topics.rs` with a
  handle-snapshot read (proving the RCU wrapper is free at the read
  site), and add a dashboard compose-with-one-plugin-section number so
  the actor-thread guest call from CR.4 is a recorded figure rather than
  an assertion. Record both in `BENCHMARKS.md`.
- `implementation.md`: CR.* status; HD.6 and DB.8 marked closed with a
  pointer here.
- Close out the source plans: `help-docs.md` and `dashboard.md` (slice
  plan) can archive once their last open slice — HD.6 and DB.8
  respectively — is ✅ *and* nothing else in them is open.
- Site sync: `nav.toml` + search, per the docs-land-on-the-Zola-site
  rule.

*doc:* the whole slice.
