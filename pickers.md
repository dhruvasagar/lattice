You're right — most pickers are ~20-40 lines each. The routing table, fuzzy
engine, and renderer are already solid. Here's what each needs:

┌──────────────────┬────────┬──────────────────────────────────────────────────────┐
│      Picker      │ Effort │                        Notes                         │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ File picker      │ Low    │ Walk dirs → RoutingPayload::OpenFile                 │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Recent files     │ Low    │ Track Vec<PathBuf> on App, already open on :edit     │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Line picker      │ Low    │ Read rope → reuse LspLocation routing                │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Register picker  │ Low    │ Walk self.registers → RoutingPayload::Register       │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Mark picker      │ Low    │ Walk self.marks → reuse LspLocation for current file │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Position history │ Low    │ Walk self.position_history → LspLocation             │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Command palette  │ Medium │ Walk self.registry.names() → RoutingPayload::Command │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Live grep        │ Medium │ Spawn rg synchronously → reuse LspLocation           │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Document outline │ Medium │ Request LSP documentSymbol → LspLocation             │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Snippet picker   │ Medium │ snippet_registry.meta_for_language() → expand        │
├──────────────────┼────────┼──────────────────────────────────────────────────────┤
│ Theme picker     │ High   │ No named theme list exists yet — needs infra first   │
└──────────────────┴────────┴──────────────────────────────────────────────────────┘

My recommendation: implement all 10 (defer theme picker — needs a named theme
registry first). I'll write an implementation plan via the writing-plans skill
now.

One clarifying question first: for live grep, should I use rg (fast, requires
ripgrep installed) or a pure Rust file walker + string search (slower but
dependency-free)?

Answer: Lets not hard code rg, there should be a configuration that configures
a specific backend rg, ag, grep, etc.
