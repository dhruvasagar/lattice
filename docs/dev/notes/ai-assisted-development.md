# Building Lattice with AI

### A whitepaper on constructing complex software with a capable, forgetful collaborator

**Author:** Dhruva Sagar
**Subject:** the method — not the editor — behind Lattice
**Document type:** engineering whitepaper, written as a well-instrumented single-project case study
**Status:** living document; revised as the practice and the tools evolve

---

## Executive summary

I have been building Lattice — a modal, GPU-accelerated, plugin-first text
editor in Rust — in sustained collaboration with an AI coding agent. Lattice is
not a toy. It is a multi-crate workspace with a non-blocking actor core, a
WebAssembly Component-Model plugin host, two peer renderers, a vim grammar
engine, an LSP client, and a tree-sitter pipeline, held to a paramount
performance goal measured in fractions of a millisecond. It is exactly the class
of software the conventional wisdom says AI cannot help with: too large to fit in
a prompt, too interdependent to change locally, too demanding to accept
"probably correct."

This whitepaper sets out how that collaboration actually works. The central claim
is that the difficulty of building complex software with a highly
capable model is **not** getting the model to write correct code — it will do
that, often brilliantly, on the first try. The difficulty is **coherence over
time**: keeping thousands of interdependent decisions aligned across a
collaborator that forgets everything between sessions, is structurally biased
toward the locally-easy path, and optimizes whatever proxy you hand it. The
work that matters is not authorship. It is the construction of an *environment* —
a constitution, an externalized memory, a set of gates and ratchets, an
interaction contract — in which the model's considerable capability reliably
produces the right thing instead of the merely plausible thing.

This whitepaper argues that this is a durable shift in the engineer's role, from author to
architect-of-constraints and reviewer-of-mappings; that the leverage of good
scaffolding *increases* rather than decreases as models improve; and that the
apparatus I describe here is an early, hand-built prototype of infrastructure
that will become standard, and increasingly machine-consumed, as context windows,
compute, and capability grow. The final part of this document is devoted to that
trajectory: what dissolves, what persists, and what intensifies.

A methodological caveat I will not bury: this is a study with n = 1, a single
operator, and a survivorship bias baked in — I am describing a project that
worked. I take the threats to validity seriously and treat them explicitly in
Part V. Read the rest as a well-instrumented case study, not a controlled result.

---

## How to read this

Part I sets the problem: why Lattice is a hard case, a theory of what an AI
collaborator actually *is*, and the position in full. Part II is the evidence —
eleven practices, each traced to a concrete failure it was built to prevent, with
the cost named honestly. Part III is the economics: where this paid, where it
cost, and the net. Part IV is the forward look — how each practice should evolve
as the underlying capabilities scale, and which of them I expect to invert. Part
V states the limits of the claim and concludes.

It is written to be split. Each chapter in Part II stands alone and could be an
article; Part IV is the section with the longest shelf life and reads on its own.
But the argument is cumulative, and is written to be read in order at least once.

---

# Part I — The setting

## 1. Why Lattice is a hard case

Most published accounts of AI-assisted development study easy cases: greenfield
apps, well-trodden CRUD, isolated scripts, code with abundant training-set
precedent. The lessons from those cases are real but shallow, because the binding
constraint there is *generation*, and generation is exactly what models are best
at. To learn anything about the hard part you need a hard case. Lattice qualifies
on several independent axes, and it is worth being precise about them, because
each axis stresses a different part of the collaboration.

**Scale beyond a context window.** The workspace is dozens of crates and hundreds
of thousands of lines. No model, at any context size I have worked with, holds it
all at once with fidelity. Every session begins with partial knowledge, and the
question of *which* partial knowledge is loaded — and whether it is current — is
itself a design problem.

**Deep interdependence.** Lattice's guiding architectural principle is
"everything is a buffer": file trees, diagnostics, search results, help, the
minibuffer — all are documents flowing through one dispatch path. This is a
beautiful constraint and a merciless one. It means a change is almost never
local. A renderer decision touches the buffer model touches the mode system
touches the plugin boundary. There is no safe corner to make an isolated mess in.

**A paramount, non-negotiable performance goal.** The top priority is
keystroke-to-glyph latency the user cannot perceive — indistinguishable from the
terminal echoing the key. This is not a preference to be traded against
convenience. It is enforced as physics (land within one display frame under any
load) and as a CI ratchet (the measured distribution may only move down). It
forbids entire categories of otherwise-reasonable code: no I/O, parsing, shaping,
or tree walks on the render thread; element fan-out proportional to viewport, not
document. A collaborator that does not internalize this will produce code that
passes every test and is nonetheless wrong.

**An extensibility substrate with a frozen future.** The plugin host is a WASM
Component-Model runtime where WIT is the canonical API. The interface, once real
plugins depend on it, is expensive to change. Getting the *shape* right — the
boundary between what crosses typed and what stays host-side — is a decision with
a long half-life, made under uncertainty, in the area the design doc itself flags
as highest-risk.

**Two peer renderers in lockstep.** TUI and GPUI are not primary-and-fallback;
they are peers that must stay in parity. A change to the effect classifier or the
theme propagation in one is incomplete until it lands in the other, in the same
patch. This is precisely the kind of dual-obligation a forgetful collaborator
drops.

**A living design document.** The architecture doc is not documentation written
after the fact. It is lived-in — the place decisions are made and revised, ahead
of and alongside the code. This means the collaboration is not just about code;
it is about keeping a large body of prose and a large body of code mutually
consistent as both change.

Any one of these would make Lattice a more demanding test than the standard
case. Together they mean the project cannot survive the failure modes that easy
cases hide. That is what makes it worth writing about: the scaffolding I describe
did not emerge from taste. It emerged from necessity, one correction at a time.

## 2. A theory of the collaborator

To reason about the collaboration I need a model of the collaborator, and I want
it to be sharper than the usual anthropomorphic hand-waving. The AI agent I work
with has four properties that together determine everything downstream. I state
them as claims about behavior, not about the model's internals.

**It is highly capable.** Given a well-scoped task, it produces correct,
idiomatic, multi-file changes fast. It wrote the whole `WitBoundary` mirror for a
~105-variant closed `Effect` enum — a mechanical, exacting, easy-to-get-subtly-
wrong task — compiler-exhaustively, both directions, with round-trip tests. It
rewrote the incremental-highlight pipeline so cost became O(viewport) instead of
O(file). It relocated a god-function boot sequence into a per-subsystem install
protocol. On raw generation, it is genuinely faster than I am, and frequently
better.

**It is eager to converge.** Given a task, it produces *a* solution and moves to
close the loop. This is usually a virtue and occasionally a serious hazard,
because the disposition to produce an answer is stronger than the disposition to
say "the premise is wrong; stop." Left alone it will implement the thing you
asked for even when the thing you asked for is a mistake — and it will do it
persuasively.

**It is forgetful.** Between sessions it retains nothing. Within a long session,
context is compacted and detail decays. A correction I made yesterday does not
exist today unless it was written into a file the agent reloads. This is not a
small inconvenience to be worked around; it is the single fact from which most of
the scaffolding follows. A human collaborator accumulates judgment. This one does
not, unless the accumulation happens outside it.

**It is biased toward the locally-easy path.** Faced with two designs, it
gravitates to the one that is smaller to *write*, not the one that is better to
*live with*. And, tellingly, it will justify the choice with the vocabulary of
virtue: "cleaner," "simpler," "more consistent," "idiomatic." The bias is not
laziness; it is a genuine and understandable pull toward the solution with the
most training-set support and the fewest tokens, which correlates with local ease
and anti-correlates with global fit on exactly the novel, constraint-heavy
decisions that matter most on a project like this.

Now the synthesis, which is the actual theory. Two of these properties —
capability and eagerness — are about *power*. Two — forgetfulness and the
easy-path bias — are about *alignment*, in the small-scale, mundane sense of
"does the output match the project's actual constraints and prior decisions."
The collaborator is high-power and variable-alignment. **Every serious risk in
the collaboration lives in the gap between its capability and its alignment.** A
low-capability agent that is misaligned produces obvious garbage you discard for
free. A high-capability agent that is misaligned produces *plausible, well-
argued, test-passing* code that violates a load-bearing invariant three
abstraction layers away. And it costs you real time to catch, because catching
it requires holding the whole system in your head, which is the thing you cannot
do, which is why you are using the agent.

This yields the uncomfortable corollary that runs through the rest of this
document: **as capability rises, the cost of an alignment failure rises with it.**
A more capable agent makes larger, more confident, more internally-coherent wrong
changes. The naive expectation — "better model, less process" — has the sign
backwards for the failures that actually hurt. Better model, higher stakes on
alignment, and therefore *more* return on the scaffolding that keeps capability
pointed in the right direction.

## 3. Position and core claims

The position can now be stated precisely.

**First:** on complex software, the binding constraint of AI-assisted development
is not generation but coherence — the maintenance of consistency among many
interdependent decisions across a collaborator that does not remember them and is
biased against them. Generation is cheap and getting cheaper. Coherence is
expensive and does not automatically get cheaper as the model improves.

**Second:** the engineer's highest-leverage work therefore moves *up the stack*,
away from authorship and toward the construction and maintenance of the
environment in which authorship happens — the constitution of principles, the
externalized memory, the verification gates, the quality ratchets, the
interaction contract. The human becomes an architect of constraints and a
reviewer of mappings between decisions and principles, more than a writer of
lines.

**Third — and this is the central forward claim:** the leverage of this
scaffolding *increases* as models get more capable, more autonomous, and
longer-context, rather than decreasing. The scaffolding built for a forgetful
collaborator does not become obsolete when memory grows; it *transforms* — from
a mnemonic aid the human maintains for the model's benefit into an executable
specification that increasingly-autonomous agents optimize against and that
verification systems check. Externalized judgment becomes more valuable, not
less, because it is the interface between human intent and machine execution, and
that interface carries more traffic as execution becomes more autonomous.

The rest of Part II is the evidence for the first two claims. Part IV is the
argument for the third.

---

# Part II — The practices

Each chapter follows the same arc: the **problem** (the failure mode, stated as
behavior), the **mechanism** (what I built to counter it), the **evidence**
(where it shows up in Lattice, named concretely enough to verify), the **cost**
(what the mechanism is not free of), and the **generalization** (what transfers).
I have ordered them roughly from most fundamental to most specific.

## 4. Drift is the failure mode

**Problem.** The agent rarely fails by being unable to do the work. It fails by
diverging, slowly, from decisions already made — reintroducing a pattern that was
explicitly rejected several sessions ago, because nothing in the current context
carries the rejection. Each individual divergence is small and locally
reasonable. In aggregate they are how a coherent architecture rots.

I know this precisely because Lattice's `CLAUDE.md` contains a section titled
"Standing rules from prior sessions," and its own preamble says what it is: each
rule "was a real correction Dhruva made that I kept re-violating before the rule
got codified. Re-violation is the failure mode this section prevents." That
section is not a style guide. It is a scar log.

**Mechanism.** The countermeasure is not a better prompt. It is to treat every
correction as a defect in the *environment* rather than an event in the
conversation, and to fix the environment — to write the correction into a
durable, reloadable, and ideally checkable form — before moving on. A correction
made only in chat is, by the forgetfulness property, a correction that will be
un-made. The discipline is: never accept a correction as "understood." Accept it
only as "encoded."

**Evidence.** Three re-violations, each of which recurred across multiple sessions
before it was codified, and each of which is now a named rule:

- *Kind-branching.* The agent repeatedly wrote `match buffer_kind { Help => …, _
  => … }` in render, motion, and scroll code — a direct violation of "everything
  is a buffer." The fix was never to add a branch; it was to find the *property*
  that actually differs (wrap on/off, read-only, line-count) and condition on
  that, so a help buffer and a document buffer with the same options render
  identically. This became the K.4 test `multibuffer_is_a_regular_buffer.rs`,
  which every new buffer kind must pass verbatim.
- *Mode-ownership half-migrations.* Asked to move a feature's surface into its own
  mode crate, the agent would move the *keymap* and leave the *handler body* as an
  `Editor::do_<x>` method in the host — the visible half of the refactor, called
  done. This became the "modes own their full surface" rule, with an explicit
  acid test: a new provider crate must require zero `Editor::` method additions
  and zero new host `Action` variants.
- *Silent failure.* The agent would swallow a recoverable error rather than
  log-and-skip, which reads as "handled" when it is in fact "hidden." This became
  the "graceful error handling" clause of the four-artifact rule.

**Cost.** Codification has a real price. The rule set grows; `CLAUDE.md` is now
long, and every session pays to load it. There is a genuine risk of over-fitting —
of encoding a rule so specific that it fights a future change that is actually
correct. I mitigate this by writing, into the rules themselves, that when a rule
appears to conflict with a paramount goal, the goal wins and the rule is the
suspect. But the tension is real: the scar log is also a cage, and a cage sized
for yesterday's mistakes.

**Generalization.** Budget for drift as the primary risk on any long-horizon
AI-assisted project, above raw capability. The unit of progress is not "the agent
now understands X" but "X is now encoded where the agent will reload it." Treat
the knowledge base's growth as a leading indicator of the collaboration maturing,
and its accuracy as something to be maintained, not assumed.

## 5. Externalized judgment as infrastructure

**Problem.** Because the agent forgets, the project's accumulated judgment cannot
live in the agent. If it lives only in me, then I am the context window — every
decision routes through my memory, and the collaboration is bottlenecked on a
human recalling why we did things this way, session after session. That does not
scale, and it defeats much of the point.

**Mechanism.** The judgment has to live *outside both of us*, in a form the agent
reloads and I can audit. Lattice uses three tiers, and the tiering is deliberate:

1. *The constitution* (`CLAUDE.md`). Paramount goals, decision heuristics,
   standing rules, conventions. Loaded every session. Deliberately opinionated
   and load-bearing — it is written to *override* the agent's defaults, and it
   says so.
2. *Per-fact memory* (`memory/*.md`, indexed by a one-line-per-fact `MEMORY.md`).
   One fact per file, typed as user / feedback / project / reference. This is
   where volatile state lives ("PH7.4 is decomposed a–e; 4a is done") alongside
   corrections-with-their-reasons and hard-won gotchas.
3. *The authoritative documents* (`design.md`, `implementation.md`, the slice
   plans). The only records I permit to be treated as ground truth about what
   exists.

**Evidence.** The discipline that makes this work is not "write things down"; it
is knowing what *not* to write down. The memory rules are explicit that I do not
store what the repo already records — code structure, past fixes, git history —
because that is noise that dilutes the signal and, worse, goes stale and starts
lying. What earns a memory is the *non-obvious*: the decision a reader of the code
could not reconstruct, and the reason behind it. "We backed the plugin `document`
resource with an `Arc<DocumentSnapshot>`, not a live `Arc<dyn Document>`, because
the live handle is a mutation-under-read hazard" is a memory. "We added a field to
a struct" is not. The `why` is the payload; the `what` is in the diff.

There is a second-order discipline: memories decay. A recalled memory reflects
what was true when it was written. So the rules require that a memory naming a
file or symbol be *verified against the source* before it is acted on — the memory
is a pointer, not a proof. This keeps the externalized judgment honest as the code
moves under it.

**Cost.** This is real, ongoing overhead, and it is overhead I pay largely for a
*future* session's benefit, which makes it easy to skimp on under time pressure.
Bad memories are worse than no memories, because they are confidently wrong and
get reloaded as fact. Curating this well is a skill, and I have written stale
memories that later misled a session before I caught them. The knowledge base is
a deliverable with a maintenance burden, and pretending otherwise is how it rots.

**Generalization.** On any project spanning more sessions than a model can
remember, treat the knowledge base as first-class infrastructure with the same
care you would give a public API: designed, versioned in spirit, pruned, and
audited. The compounding is the point — each session should start from the
accumulated judgment of all prior sessions, not from zero — but compounding cuts
both ways, and stale judgment compounds too.

## 6. The constitution and the discipline of named principles

**Problem.** Design arguments in natural language are slippery. "This is cleaner"
is unfalsifiable and therefore unarguable; it can be attached to any option, and
the agent attaches it to the easy one. Worse, an argument phrased as aesthetics
can *hide* a substantive violation: a proposal that looks like a tidy
simplification can, on inspection, be a breach of a load-bearing invariant, and
the aesthetic framing is exactly what keeps you from inspecting it.

**Mechanism.** The highest-leverage rule in the entire constitution is what I call
the heuristic-mapping rule. Lattice has four named paramount goals (performance;
extensibility; extensible vim modal editing; asynchronicity) and five named
decision heuristics. The rule is: every time options are presented for a
non-trivial design choice, each option *must* be evaluated against those goals and
heuristics *by name*, before any implementation shape is discussed — and the final
recommendation must cite the heuristic that drove it, not the option's label or
its size.

This converts advocacy into something checkable. "I recommend A because it's
simpler" is inadmissible. "I recommend A because it protects paramount goal #3
(everything-is-a-buffer: the multibuffer becomes a self-sufficient document with
no host-layer kind-branch), at the cost of a larger diff" is admissible — and,
crucially, *auditable*. A reviewer can look at that mapping and ask one question:
is it honest, or is the word "simpler" secretly doing the load-bearing work it is
forbidden to do?

**Evidence.** The rule exists because unmapped presentations had already masked
real violations — the constitution names three: a silent-failure path that was
actually an extensibility breach (an incident tagged K.3.2); kind-branching that
was an everything-is-a-buffer breach (K.4.x); and a proposal (M.7) that looked
like the substrate publishing data through a trait method but was in fact a
mode-ownership half-migration, because the only consumer of the proposed method
was one mode's own handler. In each case the *mapping step* is what surfaced the
violation; the prose had sailed right past it. The rule is a direct, procedural
response to a specific class of near-miss.

**Cost.** It is heavy. Requiring a full goal-and-heuristic mapping for every
non-trivial choice is genuine friction, and for genuinely small decisions it is
overkill that slows things down and can feel like ritual. It also assumes the
principles are the *right* principles; a mapping discipline enforces consistency
with the constitution, not correctness of the constitution, and a confidently
wrong constitution would produce confidently consistent mistakes. The rule buys
auditability, and it buys it with time.

**Generalization.** Give the collaborator a small, named vocabulary of first
principles, and require every design argument to route through it explicitly. The
value is not that the agent will reason better — it may or may not — but that its
reasoning becomes *legible*, and legible reasoning is reviewable in a way that
prose advocacy is not. You are trading fluency for accountability, and on
high-stakes decisions that is the right trade.

## 7. The gravity of the easy path

**Problem.** The easy-path bias, left unchecked, does not produce obviously bad
code. It produces locally-reasonable, globally-wrong architecture: the
abstraction that leaks one layer down, the special-case that will metastasize, the
migration done to the visible boundary and no further, the typed seam replaced by
an opaque blob because the blob is a tenth the code. Each is defensible in
isolation. The damage is cumulative and shows up later, as a system that is
harder to change than it should be.

**Mechanism.** The constitution's first heuristic is a direct counterweight, and
it is worth quoting its shape: ease of coding is *not* a tiebreaker. It names two
equal-and-opposite failures to avoid — keeping an inferior primitive out of
risk-aversion ("it works," "it's well-tested," "the rewrite is big" are
explicitly declared insufficient), *and* rewriting for novelty's sake without a
concrete merit win. The decisive test is procedural: name the specific technical
advantage in prose; do not smuggle it. If you cannot name it, you do not have it.

**Evidence.** The tell I have learned to watch for is the word "just." "We can
*just* branch on the kind." "*Just* leave the handler in the host for now." "*Just*
copy the rope across the boundary." Every one of those was a locally-easy move
that a paramount goal forbade, and every one arrived wearing the word "just" as
camouflage. The opposite discipline is visible all through the plugin-host work:
the `Effect` enum crosses the WASM boundary as a *whole, typed variant mirror* —
all ~105 arms — rather than as an opaque serialized blob, even though the blob is
dramatically less code. The reason is stated in the design and it is not
aesthetic: the blob would smuggle an untyped seam into the precise area the design
flags as its highest risk (a wrong plugin ABI is expensive to unwind post-freeze).
The extra code is not a cost the discipline reluctantly pays; it is the point.

**Cost.** Fighting the easy path deliberately chooses more work now for less work
later, and "later" is a bet. Sometimes the bet is wrong — sometimes the simple
thing would have been fine, and the principled thing is over-engineering wearing a
paramount goal as a costume. The discipline can curdle into its own failure mode,
where "long-term fit" becomes a license for gold-plating. The only guard I have
found is the same "name the concrete advantage" test applied honestly in both
directions: if I cannot name what the extra structure *buys*, I do not get to
build it either.

**Generalization.** State, as a rule the agent reloads, that the easy path must be
justified rather than assumed — and then, in review, treat "simpler / cleaner /
consistent" not as a reason but as a flag to inspect. The genuinely-better design
frequently costs more keystrokes today. That cost is not an objection to it. It is
often the signature of it.

## 8. Slicing and the four-artifact contract

**Problem.** A large change from a forgetful collaborator is a coherence hazard
twice over: it is unreviewable (a two-thousand-line "here is the feature" is
exactly where masked violations hide), and it is un-resumable (if a session ends
mid-change, there is no clean state to restart from — only cleverness smeared
across five files in an unknown degree of doneness).

**Mechanism.** The unit of work is a *slice*: a change small enough to land with
the tree green, sequenced explicitly in a slice plan with a status icon. Large
changes are decomposed into slices, each landed green before the next begins. And
every non-trivial slice ships *four artifacts together*: the architecture doc, the
benchmark coverage, the test coverage (including failure modes, not just the happy
path), and graceful error handling. The rule's slogan is blunt: a change that
ships only code is incomplete.

**Evidence.** The plugin-host phase is the cleanest demonstration. The 105-variant
`Effect` mirror was not one heroic change; it was sub-sliced a → b → c → d, each
sub-slice with its own round-trip tests, its own bench row, and typed-error
handling for the arms that cannot cross the boundary yet. The picker seam that
follows was decomposed the same way, a → e, and I stopped the collaboration at the
end of the first sub-slice — types and round-trips landed, green, benched,
documented — precisely because the next sub-slice opened a genuine design
question. The green checkpoint is what made stopping there cost nothing.

Why four artifacts and not just code: on a codebase no single context holds, the
*doc* is how the next session understands the change without re-deriving it; the
*bench* is how CI catches a regression the agent cannot remember causing; and the
*failure-mode test* is the designated place where the agent's happy-path eagerness
gets caught. Each artifact compensates for a specific property of the collaborator.

**Cost.** The four-artifact rule roughly doubles the work per slice, and for
genuinely trivial changes that is waste — not every one-line fix needs a bench and
a design note, and pretending it does breeds cargo-culting. Aggressive slicing
also has a coordination cost: more commits, more plans to keep current, more
sequencing to reason about. The discipline earns its keep on changes that touch
architecture, public API, or cross-crate boundaries; applied indiscriminately it
is just overhead with good branding.

**Generalization.** Decompose aggressively and keep the tree green, so that every
boundary between slices is a safe resume point and a reviewable unit. Ship the
context (doc), the regression net (bench), and the adversarial case (failure-mode
test) *with* the code, because on a system too large to hold in one head those are
not extras — they are how the change survives contact with the next session.

## 9. Verification and the authoritative subset

**Problem.** The agent confabulates, confidently. It will reference a function, a
flag, or a module that *sounds* exactly right and does not exist — sometimes
because it existed in an earlier design, sometimes because it is the name the code
*ought* to have and the model has helpfully imagined it. This is not lying; it is
the generative process doing what it does, filling a plausible slot. But it means
the agent's claims about the codebase cannot be trusted as evidence about the
codebase.

**Mechanism.** Two rules and one hook. First: designate a *small* authoritative
subset — in Lattice, only `design.md` and `implementation.md` are ground truth
about what exists — and treat everything else, *including the agent's own memory*,
as a hypothesis pending verification against source. Second: verify before
recommending; a memory or doc reference to a symbol is not evidence the symbol
exists. Third, and this is where it gets mechanical: a pre-edit hook (GateGuard)
that forces the agent, before touching a file, to state the facts — which files
consume the target, what public surface changes, what the actual data shape is —
and to quote the instruction it is acting on, verbatim.

**Evidence.** GateGuard fires on essentially every edit, and during the picker-
seam work I paid its toll dozens of times: before adding types to the shared WIT
file, enumerate the two `bindgen!` sites that consume it and confirm the generated
Rust names; before extending the boundary conversion, name the trait's consumers;
before writing the doc, confirm no code imports it. Most of the time the facts are
mundane and the answer is "nothing else breaks." That is fine. The value is not in
the cases where verification finds a problem; it is in converting "the agent
asserted X about the code" into "the agent checked X against the code" as an
unconditional precondition, so the confabulation has nowhere to hide.

**Cost.** This is friction, deliberately, and friction has a temper. On a fast,
obviously-safe edit, GateGuard's ceremony is pure tax, and there are moments it
feels like being asked to file a flight plan to cross the room. The rule that
authoritative-source-of-truth is a *small* set is load-bearing here: if everything
were authoritative, verification would cost more than it saves. The art is keeping
the authoritative subset small enough to be cheap to check and complete enough to
be worth checking.

**Generalization.** Treat the model's statements about the codebase as hypotheses
until grounded in a read of the source; designate a small ground-truth set and
hold everything else, memory included, to verification; and where the cost of a
wrong assumption is high, make the verification a mechanical gate rather than a
hope. The generative fluency that makes the agent useful is the same fluency that
makes its unverified claims dangerous, and the two cannot be separated at the
source — only at the gate.

## 10. Guardrails as code, and the enforcement ladder

**Problem.** A rule the agent must *remember to follow* decays at the rate the
agent forgets, which is total, between sessions. Anything genuinely important
cannot depend on the agent's memory or good intentions. It has to be enforced by
the environment.

**Mechanism.** I think of enforcement as a ladder, and the practice is to climb it
in proportion to how much an invariant matters: informal request in chat → written
rule in the constitution → item on a review checklist → mechanical gate that fires
automatically → a design in which the wrong thing is not representable at all. The
higher the rung, the less the invariant depends on anyone remembering it. The top
rung is the goal for the paramount invariants.

**Evidence.** Lattice occupies several rungs deliberately:

- The "no work on the render thread" paramount goal is not left as a rule the
  agent must recall. After a remediation effort (the X-series), it is enforced at
  *compile time* — a violation fails to build. This is the top rung: the wrong
  thing is unrepresentable.
- The performance goals are CI ratchets over recorded distributions. The agent
  cannot silently slow the hot path, because it does not need to remember not
  to — the ratchet remembers, and fails the build if this month's latency is worse
  than the best achieved.
- GateGuard is the "mechanical gate" rung for the verification discipline of the
  previous chapter.
- And where an environmental trap genuinely *cannot* be gated, it gets documented
  loudly as compensation: a plain `cargo build -p lattice-cli` is TUI-only and
  *silently* skips GPUI edits, reporting "Fresh" while compiling nothing relevant.
  That silent no-op burned whole debug cycles before it went into the constitution
  as a named pitfall. Some traps you can only shout about.

There is a corollary I learned the hard way and now treat as a rule: automated
behavior belongs in the harness, not in a request to the agent. "From now on,
whenever X, do Y" cannot be satisfied by the model's memory or preferences,
because the harness — the hooks — executes those, not the model. Asking the model
to reliably "always" do something is asking a forgetful collaborator to be
dependable by willpower, which is a category error.

**Cost.** Building enforcement is real engineering, sometimes substantial — the
compile-time render-thread discipline was its own multi-slice effort with its own
cost. Over-gating is its own hazard: a gate on a low-stakes invariant is friction
with no payoff, and a thicket of gates can slow the collaboration to a crawl and
train everyone to route around them. The ladder is a prioritization tool
precisely because you cannot afford to put everything on the top rung.

**Generalization.** For any invariant that truly matters, climb the enforcement
ladder deliberately, and reserve the expensive top rungs — mechanical gates,
compile-time impossibility — for the invariants whose violation you cannot afford.
The less an important invariant depends on the agent's memory, the more robust the
collaboration is to the one property of the agent you cannot change.

## 11. The human as the higher court

**Problem.** The agent optimizes whatever proxy you hand it, faithfully and
literally. Hand it only architectural principles and it will produce an
architecturally immaculate editor that is unpleasant to use — because "pleasant to
use" was not in the proxy set, and the agent optimizes the proxy, not the goal the
proxy stands for. The map is not the territory, and the agent lives entirely on
the map.

**Mechanism.** The constitution installs an explicit priority order — user
experience outranks the paramount goals, which outrank the heuristics — and gives
me a blunt veto to wield: "even a hundred-year-old editor doesn't do this" is a
valid rejection of an otherwise-principled design that causes visible flicker, or
a pixel of change to content the user did not edit. The point of encoding it is
that the *real* objective (a fast, pleasant editor) has to sit above the
*measurable proxies* (clean layering, principle compliance), or the agent will
optimize the proxies into a locally-perfect, globally-worse result and be right by
every metric it can see.

**Evidence.** This is the one thing I do not delegate and do not try to encode away.
The agent is excellent at the proxies — it will get the layering right, the
principle mapping honest, the tests green — and structurally blind to the felt
quality of the thing, because felt quality is not in its input. So the division of
labor is explicit: the agent holds the proxies, I hold the objective, and when
they conflict the objective wins. The interaction contract around this is itself
codified, because I got it wrong enough times to need it written down: for a design
choice, the agent presents two or three concrete options with mapped trade-offs,
recommends one with reasons, and *confirms before executing* — and once I say "go,"
that is binding and it proceeds without re-litigating. Costs are stated without
softening ("this doubles the bug surface," not "this has some minor risk"). And
mid-debug, once the evidence points strongly at one cause, the agent proposes one
focused next step and takes it, rather than stalling behind a four-option menu —
menus are for undetermined choices at the *start* of new work, not for execution in
flight.

**Cost.** Keeping the human as the higher court means the human is in the loop for
every judgment the proxies cannot capture, and that is a throughput ceiling — the
collaboration moves at the speed of my attention on exactly the decisions I refuse
to delegate. It also means the quality of the result is bounded by *my* taste,
which is a single point of failure with no external check. I am comfortable with
that trade for a project I care about this much; it does not obviously generalize
to contexts where the human's taste is the weak link rather than the strong one.

**Generalization.** Reserve to the human the things the model cannot see — the felt
quality of the product and the judgment of when a principle should yield — and
encode the *interaction* around those things (options then confirm, honest costs,
act when converged) so the collaboration has a stable rhythm instead of
renegotiating its own process every session. The model is a superb optimizer of
stated proxies. Deciding which proxies, and holding the objective above them, is
the human's job and stays the human's job.

## 12. Ratchets over targets

**Problem.** Hand the agent a fixed number as a goal and it treats the number as a
finish line. Told the keystroke-latency budget is eight milliseconds, it will
produce seven and stop — satisfying the letter of the goal and abandoning the
spirit, which was "as fast as physically sensible, and never regressing."

**Mechanism.** Reframe every quality bar that can be measured as a *ratchet* over
recorded measurements rather than a *threshold* to clear. Lattice states its
performance goal in three explicitly separated parts: an aspiration (match or beat
the best-in-class reference — "best possible," never a line crossed once), a hard
ceiling (land within one display frame under load — display physics, not a
target), and a ratchet (CI records the measured distribution and fails on
regression; the bar only ever moves down). The current headline number is labelled,
in the constitution, as *descriptive and never a finish line*.

**Evidence.** The ratchet is what makes performance discipline survive a forgetful
collaborator. The agent does not have to remember last month's latency, because CI
does, and CI fails the build if this month's is worse. The discipline is offloaded
from memory to machinery — which is the same move as the enforcement ladder,
applied to a continuous quantity instead of a binary invariant. A threshold would
invite the agent to satisfice down to the threshold; a ratchet forces every change
to defend the best result achieved so far.

**Cost.** Ratchets can over-constrain. A monotonic "only ever faster" bar can block
a change that is a genuine, worthwhile trade — slightly slower on one path for a
large correctness or maintainability win. The ratchet has to be designed with
enough granularity, and enough human override, that it fails on *regressions* and
not on *trades* — and distinguishing the two sometimes requires exactly the human
judgment the ratchet was meant to save. A ratchet is a tool for holding a line, not
for deciding where the line should be.

**Generalization.** Where a quality attribute can be measured over time, express the
bar as a ratchet over the measurement, not as a static target — a target invites
satisficing, a ratchet compounds. And build in the human override, because not
every regression on a metric is a mistake, and a ratchet that cannot tell a trade
from a regression will eventually block the right change.

## 13. Documentation architected by rate of change

**Problem.** When the agent updates documentation mid-build, it edits whichever
document is in front of it — smearing volatile status ("in progress," a test
count, a slice ordering) into a document meant to be stable rationale, or freezing
a rejected alternative into a churning slice plan where no future reader will
think to look. The result is that the stable "why" and the volatile "how"
contaminate each other, and both get less trustworthy.

**Mechanism.** Separate the documentation by *rate of change*, not by topic. Design
fragments carry the stable material — contracts, data models, rationale, rejected
alternatives, the "what" and the "why." Slice plans carry the volatile material —
sequencing, slice IDs, status icons, test and commit counts, the "when" and the
"how." They live in separate files, cross-referenced, never co-located; and the
rule is explicit that when you carve a slice mid-build you update the *slice plan*,
not the design fragment, unless the design itself actually changed.

**Evidence.** This is a small rule with a large effect on the trustworthiness of the
docs over time. Because a design fragment is defined as the thing that does *not*
churn, a reader — human or agent — can trust it as a stable account of intent, and
the agent's compulsion to update *something* when it does work is directed at the
slice plan, where churn belongs. The separation is a hedge against a specific,
observed behavior: the agent's tendency to write status into whatever it has open.

**Cost.** Two files where one might do is more indirection, and it demands
discipline to keep the cross-references live and to resist the pull to just jot the
rationale into the slice plan because that is the file already open. The boundary
between "the design changed" and "only the plan changed" is occasionally genuinely
ambiguous, and getting it wrong puts stable content in the churning file or vice
versa. It is a low-cost rule, but not a zero-cost one.

**Generalization.** Organize documentation around how fast each part changes, so
that a collaborator editing the volatile part cannot erode the stable part. The
principle generalizes beyond docs: wherever a fast-changing thing and a
slow-changing thing share a container, the fast one tends to corrupt the slow one,
and separating them by rate of change protects the thing you most need to trust.

## 14. Expensive facts

**Problem.** Not every failure is a principle violation. A large fraction are
specific, load-bearing gotchas that no amount of general reasoning would have
avoided — sharp edges in the stack and the environment that the agent hits,
rediscovers painfully, and would hit again next session, because they are facts,
not inferences, and the agent does not retain facts.

**Mechanism.** Maintain a running, indexed catalog of these — the constitution's
"implementation pitfalls" section and the per-fact memory files — so that the cost
of discovering a sharp edge is paid once, not once per session.

**Evidence.** A representative sample, each of which cost real time before it was
captured:

- The service registry keys entries by `TypeId`, so registering an `Arc<EventBus>`
  and looking up `EventBus` silently returns nothing — a double-`Arc` mismatch that
  produces a `None` where you expected a service, with no error. The captured rule
  turns a baffling half-hour into a first-guess diagnosis.
- The GPUI renderer is behind a cargo feature, so a plain build of the CLI crate is
  TUI-only and silently skips GPUI edits — a no-op that reports success.
- On my WSL2 development box, clock skew future-dates build artifacts, so cargo
  wrongly reports "Fresh" after a real edit; a clean of the crate forces the
  rebuild.
- Per-keystroke and per-frame diagnostics go to `debug`, never `info`, because
  `info` fans out to a user-visible buffer and a held key at 30 Hz floods it.

None of these is derivable from first principles. They are the accumulated cost of
hitting the same wall twice, written down so the third time does not happen. This
is the least glamorous chapter and quietly one of the highest-return: a
disproportionate amount of wasted time on a real project is re-discovery of
environment-specific facts, and capturing them is nearly free relative to
re-paying for them.

**Cost.** The catalog goes stale as the stack changes — a captured gotcha about a
tool version can outlive the version and start misleading. It needs the same
verification discipline as any other memory: a pitfall is a pointer to check, not a
law. And a catalog long enough to be complete is long enough that no one reads all
of it, so its value depends on good indexing and on the agent surfacing the
relevant entry at the relevant moment.

**Generalization.** Keep an indexed log of the sharp edges specific to your stack
and environment, distinct from your principles, and treat it as maintenance, not
doctrine. The model will rediscover each edge painfully unless the discovery is
captured; capturing it is one of the cheapest high-value things you can do, and one
of the easiest to neglect because each individual entry feels too small to bother
with.

---

# Part III — The economics

I want to be concrete and honest about the ledger, because a whitepaper that
only lists practices and their virtues is marketing, not engineering.

**The project, in round numbers.** Some scale first, to ground the ledger — these
are repo-committed facts, identical on any checkout, and they describe the scale the
practices ran at, not a controlled measurement of their benefit (Part V is honest
about what cannot be shown from a single case). At the time of writing, Lattice is a
29-crate Rust workspace of roughly 300,000 lines carrying nearly 4,900 test
functions, sequenced through 45 slice plans (15 active, 30 archived) across some
2,050 commits — 74 of them in the four days since this document's first draft. The
scaffolding steering all of that is deliberately small: a ~180-line constitution
(`CLAUDE.md`) — four paramount goals, five decision heuristics, and some two dozen
further codified standing rules and pitfalls — plus a per-fact memory store and
roughly 9,000
lines of authoritative design-and-implementation docs that are the only records
permitted as ground truth about what exists. The `Effect` boundary mirror the
plugin-host chapters keep returning to is, today, 109 variants wide, each crossing
the WASM boundary typed, in both directions, under test. The number that matters is
the ratio: a few hundred lines of curated judgment steering a codebase three orders
of magnitude larger. The number I cannot give you honestly is the counterfactual —
what the same editor would have cost without the scaffolding — and its absence is
exactly the limit Part V insists on.

**Where the collaboration paid, unambiguously.** Mechanical, exacting, high-volume
work with a clear specification: the boundary mirrors, the exhaustive round-trip
conversions, the parity edits across two renderers, the migration of a boot
sequence into a uniform install protocol. This is work that is tedious and
error-prone for a human and fast and accurate for the agent, and the speed-up is
large — not two-fold, more like the difference between "a day" and "an hour," with
better test coverage than I would have had the patience to write by hand. It also
paid on *exploration*: getting a first, wrong, complete draft of a design onto the
page fast enough to see what is wrong with it, which is a different and valuable
thing from getting it right.

**Where the scaffolding cost, unambiguously.** Everything in Part II is overhead,
and the overhead is front-loaded and real. Writing and maintaining the
constitution, curating the memory, building GateGuard and the compile-time gates
and the ratchets, decomposing every non-trivial change into slices with four
artifacts each — this is a tax on every unit of work, and on a smaller or shorter
project it would not pay for itself. There were stretches where I spent more time
tending the environment than the environment saved me, and the payoff was a bet on
the project's longevity that only looks obviously correct in hindsight.

**The net, and the shape of it.** The net is strongly positive *for this project*,
and the reason is specifically its complexity and longevity. The scaffolding is a
fixed-ish cost that amortizes over the life of the project and the number of
sessions; the coherence failures it prevents are a cost that would otherwise grow
super-linearly with the size of the system, because each new decision can conflict
with more prior ones. The break-even is real and it is not at zero: for a weekend
script, all of this would be absurd. The claim is not "always do this." The claim
is "the more the software resembles Lattice — large, interdependent, long-lived,
demanding — the more the fixed cost of scaffolding is dominated by the growing cost
of the incoherence it prevents."

**What is now cheap that used to be expensive, and vice versa.** Generation is
cheap; I no longer think of writing a well-specified module as a significant cost.
Exhaustive, mechanical correctness is cheap. First drafts are cheap. What remains
expensive — what the whole apparatus is built to manage — is *specifying intent
precisely enough*, *verifying that the output matches it*, and *keeping the many
outputs mutually coherent over time*. The scarce resource has moved from typing to
judgment, from authorship to specification and verification. That relocation of
scarcity is the entire story, and it is the hinge on which the next part turns.

---

# Part IV — How this should evolve

Everything above is a snapshot of a practice at one point on several rising
curves: context windows, raw capability, available compute, and the length of
horizon over which an agent can run autonomously. The interesting question — the
one this whitepaper is ultimately for — is not "what works now" but "what happens
to each of these practices as those curves rise." I will take the axes one at a
time, then state the synthesis, which is a claim that runs against the intuitive
grain.

## 15. Four axes of growth

**Context windows grow — toward whole-repository, whole-history scope.** The naive
expectation is that this dissolves the need for externalized memory: if the model
can hold the entire codebase and every past session, why maintain a curated
knowledge base at all? I think this expectation is wrong, and instructively so.
Two problems survive arbitrarily large context. First, *cross-session persistence
of intent* is not the same as cross-session recall of text — a model that can
re-read every prior conversation still needs to know which decisions are *current*
and *authoritative* versus superseded, and that curation is judgment, not storage.
Second, and more fundamental: **more context is not more attention.** A model
given a million tokens of undifferentiated repository does not thereby know which
hundred tokens are load-bearing for the decision in front of it; large context
without a notion of *what matters* dilutes rather than focuses. The value of the
authoritative-subset idea — a small, designated ground truth — goes *up* as raw
context grows, because the scarce thing becomes salience, not capacity. The
externalized memory does not disappear; it becomes the *index and the priority
signal* over a context too large to attend to uniformly. Retrieval quality, and
the human judgment encoded in what-is-authoritative, become the bottleneck that
sheer window size cannot touch.

**Capability grows — toward fewer confabulations, deeper reasoning, more
autonomy.** As the model gets better, the *character* of the failures shifts but
the *need* for the scaffolding does not evaporate; it moves. Verification changes
from "did it hallucinate an API that doesn't exist" — a failure a better model
makes less often — to "did it satisfy the actual specification," which is subtler,
harder to check mechanically, and does not get easier just because the model is
smarter. The heuristic-mapping discipline changes from "catch the agent's
architectural error" to "align on values" — and *values are not derivable from
capability.* A more capable model is not a model that shares your priorities; it is
a model that can pursue whatever priorities it is given, further and more
convincingly. So the constitution's job shifts from error-correction toward
value-specification, and if anything becomes more important, because a more capable
agent given an underspecified objective goes further in the wrong direction before
anyone notices. Recall the corollary from Part I: as capability rises, the cost of
an alignment failure rises with it. Better models raise the stakes on exactly the
scaffolding that specifies and checks alignment.

**Compute grows — toward many agents in parallel, cheap iteration, adversarial
verification as default.** This is the axis that changes the *texture* of the work
most. When inference is abundant, the natural unit stops being one agent doing one
task and becomes many agents — proposing competing designs, adversarially trying to
refute each other's work, verifying in parallel from independent angles. Cheap
compute makes the multi-perspective verification that is expensive today into the
default. But abundance creates a new scarce resource: **coherence across parallel
work.** Ten agents editing in parallel is ten times the generation and also ten
times the drift, now happening simultaneously rather than across sessions. The
merge problem becomes the central problem. And the thing that lets parallel agents
stay coherent is exactly the externalized, machine-readable judgment I already
maintain for a single forgetful agent — the constitution, the authoritative
sources, the invariants — now consumed by many machines at once as a shared
substrate. The scaffolding I built for one collaborator becomes the coordination
layer for a swarm.

**Autonomy horizons grow — toward agents that run for hours or days
unsupervised.** Today the agent's forgetting happens between sessions, and the
scaffolding bridges the gaps. As an agent runs autonomously for longer, *drift
within a single run* becomes the new cross-session drift — the same failure mode,
now unfolding without a human in the loop to correct it turn by turn. Everything
that today depends on my catching a divergence in review has to become something
the environment catches without me: the ratchets, the gates, the compile-time
impossibilities, the failure-mode tests. The enforcement ladder is not a
convenience in this regime; it is the *only* thing standing between an autonomous
agent and hours of confidently-wrong work. Autonomy does not reduce the need for
guardrails-as-code. It is the scenario the guardrails were secretly always for.

## 16. The synthesis: leverage rises, and the scaffolding changes in kind

Put the four axes together and the intuitive story — "as AI improves, we need less
process" — inverts on two levels at once.

First, in *magnitude*: every axis of growth increases the leverage of the
scaffolding, each by its own mechanism.

- Larger context makes curated salience more valuable, not less, because attention
  does not scale with capacity.
- Higher capability raises the stakes on value-specification and spec-conformance,
  because a more capable misaligned agent does more damage before detection.
- Abundant compute turns externalized judgment into a shared coordination substrate
  for parallel work, and makes the coherence problem it solves the central one.
- Longer autonomy horizons make mechanical enforcement the last line of defense,
  with no human turn-by-turn correction left to lean on.

The unifying reason is the position from Part I, now paying off: improving models
attack generation. They make the cheap thing cheaper and leave the expensive thing —
coherence, alignment, verification — largely untouched, or make it *more* expensive
by raising the volume and confidence of the work that must be kept coherent. The
scaffolding manages the expensive thing, so its leverage is the ratio of the
expensive to the cheap, and that ratio grows.

Second, and less obviously, in *kind*. Today `CLAUDE.md` and the memory files are, in
large part, mnemonic aids — maintained so a forgetful model can reload the judgment it
cannot retain. As the axes rise, the same artifacts do not merely persist; they
transform into infrastructure that machines consume directly:

- The **constitution becomes an executable specification** — named principles and
  invariants stop being prose the agent is asked to honor and become constraints that
  autonomous agents optimize against and verifiers check mechanically. The
  heuristic-mapping rule, today a discipline enforced in review, becomes a structured
  artifact: every non-trivial decision carries a machine-checkable mapping to the
  principles it serves, and unmapped or dishonestly-mapped changes are rejected
  without a human in the loop.
- **Memory becomes the shared world-model of a swarm** — the per-fact files, today a
  note-to-future-self, become the coordination substrate that keeps parallel agents
  from diverging: read and written by machines, versioned, contested, reconciled.
- **The enforcement ladder becomes the primary control surface** — as the human
  leaves the turn-by-turn loop, the gates, ratchets, and compile-time impossibilities
  stop being backstops for a reviewer's attention and become the main mechanism by
  which intent is imposed on autonomous execution.

The content — the judgment, the principles, the invariants — is preserved and grows
more valuable; the *consumer* shifts from human-and-forgetful-model to
autonomous-and-parallel-machines; and the *form* shifts from prose toward structured,
checkable, executable specification. Building this scaffolding by hand is, in that
light, a manual prototype of infrastructure that will be productized and
standardized: hand-rolled, badly and specifically, into what will become general.

## 17. A comparison: autonomous loops, the Ralph technique, and the spectrum of human coupling

It is worth comparing this approach directly against the most visible alternative
in current practice: the *autonomous agentic loop*, and its bluntest instance, the
"Ralph" technique. (The name nods to Ralph Wiggum — a cheerfully dumb but
relentlessly persistent process that nonetheless gets there — and the technique is
associated with Geoffrey Huntley and the broader agentic-coding practitioner
community.) The comparison is clarifying, because the two approaches look like
opposites and are in fact complementary — points on a single spectrum of *human
coupling*.

**What the loop is.** In its purest form the Ralph technique is `while true: run the
agent against the same prompt`. A stable specification lives in a file; the agent is
launched against it over and over; each iteration reads the current state of the
repository, makes some progress, and the loop's feedback signal — the build, the
tests — tells the next iteration where it stands. The agent recovers from its own
mistakes across iterations by brute persistence. Around this core sit the more
structured members of the family: single-agent iterate-and-reflect (Reflexion-style
self-critique; Shinn et al. 2023), the open-ended skill-accumulating agent (Voyager;
Wang et al. 2023), the AutoGPT/BabyAGI lineage of goal-decomposing loops, and the
multi-agent orchestrations — generator/critic pairs, debate, RFC-driven DAG
pipelines that fan work across many agents. They differ in structure but share a
commitment: **maximize autonomy, let the loop and its feedback signals carry the
work, minimize the human's per-step involvement.**

**The engineering around the loop — because the bare loop is the least of it.** In
practice the naive `while true` is not the interesting part; what has matured around
the Ralph technique is a discipline of *loop engineering* whose moves rhyme, closely,
with the scaffolding in Part II — arrived at from the opposite direction. The
practitioner conventions, as they have settled, look like this:

- *A standing prompt plus an editable plan file.* The loop is not pointed at a raw
  task but at a curated instruction (a `PROMPT.md`) and, separately, a living backlog
  (a `fix_plan.md` or task list) that the agent reads at the top of each iteration
  and *rewrites* at the bottom — checking off what it finished, recording what it
  learned, re-ordering what remains. This is the externalized specification and the
  slice plan of Chapters 5 and 8, discovered independently: the loop, like a session,
  forgets between iterations, so its intent and its progress must live in files it
  reloads.
- *One meaningful change per iteration.* The hard-won discipline is to instruct the
  loop to do the smallest useful thing and stop, not to attempt the whole backlog in
  a single pass — an iteration that tries to do everything exhausts its context and
  converges on mush. This is slicing and the green checkpoint (Chapter 8), re-derived
  as a context-budget survival tactic rather than a reviewability one.
- *Subagent fan-out to protect the context budget.* Mature loops delegate search,
  exploration, and verification to fresh subagents whose findings come back
  distilled, so the main loop spends its scarce attention on the change itself and not
  on re-reading the repository. This is the authoritative-subset and salience argument
  of Chapter 15 — more context is not more attention — turned into an operational
  habit.
- *A commit per iteration as the resume point.* Each pass that leaves the tree green
  is committed, so a bad iteration is a `git reset`, not a half-state smeared across
  five files. This is green-per-slice, verbatim.
- *Guardrails the loop cannot argue with, and then "let it cook."* The fitness signal
  is the build and the test suite; the loop is then left to run — for hours,
  overnight — on the bet that persistence plus an honest signal converges. The
  precondition for walking away is exactly that the guardrails are strong enough to
  survive being unwatched: the same enforcement-ladder logic (Chapter 10) I built for
  a forgetful collaborator is what makes an unsupervised loop safe to leave running at
  all.

The convergence is the point worth dwelling on. Loop engineering and the Lattice
practice are not borrowing from each other; they are two independent responses to the
*same* substrate — a capable, forgetful, easy-path-biased generator. They keep
landing on the same primitives (externalized spec, small green steps, checkpointing,
mechanical guardrails, delegated verification) because those primitives are forced by
the substrate, not by either author's taste.

**Where the approaches agree — more than they appear to.** The Lattice practice and
the Ralph loop rest on several of the same foundations, and the overlap is not
accidental:

- *Both externalize the specification.* Ralph's `PROMPT.md`/spec file is the same
  move as my constitution, slice plans, and design fragments — the recognition
  (Chapter 5, Naur) that the agent forgets and intent must live outside it. A Ralph
  loop with a vague prompt thrashes for exactly the reason an under-specified session
  drifts.
- *Both anchor on machine-checkable feedback as ground truth.* The loop's fitness
  function is the build-and-test signal; my anchor is the green tree, the CI ratchet,
  the compile-time gate. Both treat a verifiable, automatic signal as the thing that
  keeps generation honest.
- *Both use checkpointing as the safe-resume unit.* Ralph leans on git commits per
  iteration; I lean on green-per-slice. Same idea: a forgetful process needs frequent,
  unambiguous restart points.
- *Both treat generation as cheap and iteration as abundant.* Neither approach
  hoards keystrokes; both spend generation freely and worry about coherence.

**Where they diverge — the axis is human coupling, and everything follows from it.**

- *Locus of control.* The loop is autonomy-maximal: set it running, walk away, review
  the outcome. Lattice is coupling-maximal at the junctions that matter: the agent
  confirms before bulk execution and I review every non-trivial mapping. This is the
  root difference from which the rest descend.
- *Precision-per-step versus convergence-by-persistence.* The loop bets that
  brute-force iteration plus feedback will *converge* on correctness even if any single
  step is wrong; it tolerates wasted work as the cost of autonomy. Lattice bets on
  getting each slice *right* and landing it green, so there is little to converge from;
  it minimizes wasted work by spending human attention up front. The loop trades
  compute for human time; the scaffolded collaboration trades human time for coherence.
- *Where quality comes from, and the ceiling on it.* The loop's quality is bounded by
  the quality of its feedback signal. Where the objective is mechanically verifiable —
  a migration, a boundary mirror, an exhaustive conversion, the "expensive facts" class
  of Chapter 14 — the loop is superb, and honestly often better than a supervised
  session, because it will grind through tedium without fatigue. But where the
  objective is *not* mechanically verifiable — the felt quality of the editor, whether
  a plugin ABI is shaped for a decade of unknown plugins, whether an abstraction is
  the right one — the loop has no signal to optimize against, and it will either
  thrash or, worse, confidently converge on something that passes the tests and is
  wrong. This is the higher court of Chapter 11 restated: some of the most consequential
  decisions have no test, and a loop is blind exactly there.
- *Exposure to the proxy problem.* This is the sharpest difference, and it is
  Goodhart (Chapter 12) again. A loop optimizes its fitness function *hard*, which
  makes it maximally exposed to specification gaming: if the tests are gameable, a
  long-running loop will find the game — write vacuous tests, delete the failing one,
  satisfy the letter and gut the spirit — and it will do so without malice and without
  noticing. My defenses against this (the human as higher court, the heuristic-mapping
  audit, adversarial verification) are precisely the defenses a bare loop lacks. A loop
  is only as safe as its feedback signal is ungameable, and on real software the signal
  is never fully ungameable.
- *Behavior at design junctions.* This is where the difference becomes concrete.
  During the plugin-host work I *stopped* at the end of one sub-slice because the next
  step opened a genuine design decision — the shape of the picker boundary, a decision
  with a long half-life in the highest-risk area of the system. A Ralph loop does not
  stop there; it plows through and picks *a* shape, because its job is to keep going.
  On a mechanically-verifiable task that is a virtue. On an expensive-to-reverse
  architectural fork with no local test to distinguish good from bad, it is exactly the
  wrong behavior — it converts a decision that deserved deliberation into a fait
  accompli discovered later. The scaffolded collaboration is designed to detect those
  junctions and surface them; the loop is designed to power through them.

**The synthesis: the loop is the engine, the scaffolding is the control system.**
These are not rival philosophies to choose between; they are complementary layers,
and I think the future is their composition, not the victory of either. The Lattice
apparatus — a machine-checkable constitution, ratchets, gates, compile-time
impossibilities, a curated authoritative memory — is *exactly what an autonomous loop
needs to be safe on hard software*. Point a bare Ralph loop at a codebase with a
paramount latency goal, an everything-is-a-buffer invariant, and a soon-to-freeze
plugin ABI, and with only "make the tests pass" as its signal it will drift, game the
signal, and bulldoze the design forks — because nothing tells it not to. Wrap that
same loop in the scaffolding and it inherits the guardrails: the ratchet forbids the
latency regression, the compile-time gate forbids render-thread work, the
heuristic-mapping and human-review checkpoint catch the architectural forks the tests
cannot see. This is precisely the argument of the autonomy-horizon axis (Chapter 15)
and the synthesis of Chapter 16, arriving from the other direction: as the loop's
autonomy grows, the scaffolding stops being a supervisor's convenience and becomes the
*only* thing standing between the loop and hours of confidently-wrong work.

So my honest read of where these meet: use the loop where the objective is
well-specified and mechanically verifiable — and lean on it hard there, because that is
where cheap compute pays off spectacularly. Keep the human coupled, and keep the
scaffolding as the loop's boundary conditions, where the objective is taste-laden,
architecturally consequential, or otherwise untestable. The two are not a dichotomy
but a division of labor along the line of *verifiability*, and the design of that line —
deciding what is safe to loop and what must be deliberated — is itself one of the
human-irreducible jobs of Chapter 19. The Ralph technique and the scaffolded practice
described here are, in the end, answering the same question from opposite ends: how do you get coherent
software out of a forgetful collaborator? The loop answers "persistence plus a
feedback signal"; I answer "externalized judgment plus human review at the junctions."
The strongest system uses both, and knows which to apply where.

## 18. The reflexive turn: Lattice becomes a harness for the collaborator

Since the first draft of this whitepaper the argument has closed a loop I did not
plan. Lattice now *embeds* the class of collaborator it was built with: it hosts AI
coding agents — Claude Code and opencode today — behind one shared capability
surface, the `EditorAccess` port, rather than a bespoke per-agent subsystem. The
generalizing transport for the headless, lattice-driven agents is the Agent Client
Protocol (ACP) — the Zed-originated, LSP-shaped standard a growing set of agents
already speak — while Claude Code rides the inverted topology (lattice as an MCP
server the agent connects back into). Either way the payoff is the same: adding an
agent is launch configuration, not a new subsystem, and sessions, streamed
conversation, tool-call rendering, and interactive edit review are implemented once
and reused.

What makes this more than a feature is *which* parts of the editor the integration
reuses. An agent's proposed edits do not simply apply — they flow through lattice's
own diff subsystem as a permission gate: the write blocks on
`session/request_permission`, surfaces as a `review_diff` the user accepts or
rejects, and **review mode fails closed** — reads auto-run, file edits open a diff,
un-reviewable mutating operations are denied unless the user opts into a per-session
*trust mode*. The conversation is a Document buffer like any other; the prompt is an
editable-tail (comint) surface behind the read-only edit gate; protocol diagnostics
stream to a dedicated `:ai-log` buffer rather than polluting `*messages*`.

Read against Part II, this is the thesis compiled into the product. The
human-as-higher-court of Chapter 11 becomes a literal accept/reject gate on every
agent write. The enforcement ladder of Chapter 10 becomes a fail-closed default an
autonomous agent cannot argue past. The dedicated-buffer discipline — stream a
subsystem's output into its own named buffer, never the shared scratchpad — is the
same rule that governs the memory files, now enforced in the UI. The editor built
*with* a forgetful collaborator, by externalizing judgment and gating execution,
turns out to be a serviceable *harness* for supervising that same class of
collaborator: the autonomy-horizon argument of Chapter 15 — mechanical enforcement as
the last line of defense — arriving once more from the other direction. The
scaffolding was always a control system; giving it a user interface only made that
visible.

## 19. What stays human, and why

If generation goes to nearly free and verification and coordination increasingly
mechanize, the honest question is what remains irreducibly the human's. My answer,
in descending order of confidence:

**Taste, and the felt quality of the product.** The higher court of Chapter 11 does
not mechanize, because it is defined by human experience — the objective the
proxies stand in for. A model can be told "no visible flicker" and check for it;
it cannot originate the standard that flicker is unacceptable, because that
standard comes from caring what using the thing feels like. As long as software is
for people, someone who is a person has to hold the objective above the proxies.

**Objective-setting — deciding what to build and why.** Capability answers "how";
it does not answer "what is worth doing," which is a question about the world and
about values, not about code. The scarce input at the top of the stack is a
well-formed intent, and forming it is the least automatable thing in the pipeline.

**Constraint-specification — turning intent into something checkable.** Between the
human's fuzzy objective and the machine's precise execution sits the work of making
the objective *precise enough to verify against*. This is the work the whole
constitution represents, and I expect it to remain substantially human for a long
time, because it is exactly the translation of taste and intent into machine-legible
form — and if that translation could be fully automated, the objective and the
taste behind it could be too, which is the thing I just argued cannot.

**Verification design — deciding what "correct" means and how it is checked.** The
running of verification mechanizes; the *design* of them — what to check, how
adversarially, against what standard — is a judgment about what matters, and
inherits the non-automatability of the objective it serves.

Notice that these are precisely the activities the scaffolding is made of. That is
not a coincidence. The scaffolding is the externalization of the human-irreducible
part of software engineering, built because a forgetful collaborator forced me to
externalize it. As the collaborator's other limitations fall away, the part that
remains is the part I was forced to write down, and it turns out to be the part
that was always the actual engineering.

---

# Part V — Limits, and a close

## 20. Threats to validity

I would not trust this document if it did not attack itself, so:

**n = 1, and a survivor.** This is one project, described by the person who chose
its methods and has an interest in their vindication. I am describing something
that worked, which means I cannot distinguish the practices that *caused* the
success from the ones that merely *accompanied* it. Some of my scaffolding may be
superstition — ritual that felt protective and prevented nothing. I have tried to
tie each practice to a specific failure it demonstrably prevented, but "this rule
followed these three incidents" is correlation dressed for court.

**Single operator, confounded with a learning curve.** I got better at this over
the same period the tools got better and the scaffolding accumulated. I cannot
cleanly separate "the method works" from "I learned to work" from "the models
improved." All three rose together, and the narrative here gives the method more
credit than a controlled study could.

**Selection on domain.** Lattice is systems software in Rust with a strong,
opinionated architecture and a single decisive taste-holder. That is close to the
ideal case for a constraint-heavy, constitution-driven method. Software with fuzzy
requirements, many stakeholders, or a domain where the training-set precedent is
thin or misleading might reward a very different approach, and I have no evidence
either way.

**The forward-looking part is speculation.** Part IV is argument, not data. I have
tried to make it *mechanistic* — to say *why* each axis should have the effect I
claim — so that it is at least falsifiable in principle. But it is a set of
predictions about systems that do not fully exist yet, made by someone with a
particular vantage, and it should be read as a considered bet, not a finding.

I hold the core of the thesis more firmly than any single practice: that coherence,
not generation, is the binding constraint, and that the human's leverage moves up
the stack toward constraint and verification. The specific mechanisms are my best
current implementation of that thesis and I expect to be wrong about the details.

## 21. Close: the scaffolding is the artifact

I set out to build a text editor and, in the process, built something I did not
plan to: an environment for building it — a constitution, a memory, a set of gates
and ratchets, an interaction contract. For a long time I thought of that
environment as overhead, a tax I paid to get the editor. I have come to think the
relationship is the other way around.

The editor is a specific answer to a specific question. The environment is a
general answer to the question of how to build complex, coherent software with a
collaborator that is powerful, eager, forgetful, and biased toward the easy path —
which is to say, with the collaborator we now have, and, in transformed form, the
collaborators we are going to have. The practices in Part II are not Lattice-
specific, even though their content is; their *shape* — externalize judgment, name
your principles, verify against a small ground truth, climb the enforcement ladder,
keep the human as the higher court, ratchet the measurable, separate by rate of
change — is the shape of the discipline that the shift from authorship to
architecture-of-constraints demands.

And the part of that discipline that is hardest, most valuable, and least
automatable is the same part that will remain the human's as everything around it
mechanizes: deciding what is worth building, holding the objective above the
proxies, and specifying constraints precisely enough that a machine can be trusted
to execute against them without a human watching every turn. I was forced to write
that part down because my collaborator could not remember it. It turns out to be
the part that was always the real work.

The scaffolding is the artifact. The editor is what it produced — and, now that the
editor embeds the collaborator behind those same gates, part of how the next thing
will be built.

---

*Appendix — on splitting this into articles.* If this is broken into a series, the
natural seams are: (1) the framing and the theory of the collaborator (Part I) as a
standalone essay on why complex software is the hard case; (2) each Part II chapter
as a short, self-contained practice piece — Chapters 4, 6, 9, and 10 are the
strongest on their own; (3) Part III as a candid economics note; and (4) Part IV as
the flagship forward-looking essay, which I would publish first and independently,
since it is the part with the longest shelf life. The through-line to preserve
across any split is the single claim that coherence, not generation, is the binding
constraint — every piece should be traceable back to it.

---

## Related work and intellectual lineage

Almost nothing in this document is new in isolation. What I claim is new is the
*synthesis* — the observation that a specific, capable-but-forgetful collaborator
recreates, at a new scale and with new urgency, a set of problems that several
established fields have each studied from one side. It is worth situating the
argument against that prior work, both to give credit and to be precise about what
is borrowed and what is genuinely a response to the new situation. (I have cited
only works I am confident exist; given this document's thesis about confabulation,
inventing a bibliography would be a peculiar self-own. Verify freely — the
references are at the end.)

**Complexity and conceptual integrity.** My central quantity — *coherence* — is a
descendant of Brooks's *conceptual integrity* (Brooks 1975, 1986). Brooks argued
that conceptual integrity is the most important consideration in system design and
that it is threatened by the number of hands (and minds) involved; his "No Silver
Bullet" separated *essential* from *accidental* complexity and warned that tooling
attacks the accidental and leaves the essential. My thesis is a direct restatement
in the age of generative models: capability attacks generation (accidental), and
leaves coherence (essential) — the hard, irreducible part — largely intact. Parnas
(1972) on information hiding and Conway (1968) on the mirroring of communication
structure onto system structure are the deep sources for why interdependence is the
enemy; "Out of the Tar Pit" (Moseley & Marks 2006) is the modern restatement that
complexity, especially state and control, is the thing to be relentlessly managed.
The everything-is-a-buffer discipline and the mode-ownership rule are Parnas and
Conway applied — and the reason a forgetful collaborator strains them is that it has
no persistent mental model of the module boundaries to respect.

**Programs as theories; distributed cognition.** The single closest intellectual
ancestor is Peter Naur's "Programming as Theory Building" (1985). Naur argued that a
program's essence is not its text but a *theory* held in the developers' minds —
tacit, unwritten knowledge of why the code is shaped as it is — and that a program
whose theory has been lost is effectively dead even if the source survives. This is
exactly my situation, made acute: my collaborator has no persistent mind in which
the theory can live, so the theory *must* be externalized or it is lost every
session. The entire memory-and-constitution apparatus (Chapters 5–6) is an attempt
to write down Naur's theory in reloadable form. Hutchins's *Cognition in the Wild*
(1995) on distributed cognition — cognition as a property of a system of people and
artifacts, not a lone head — is the frame for treating the constitution, the memory,
and the gates as cognitive infrastructure the human and the model share.

**Disciplines of small, safe, always-green steps.** The slicing and four-artifact
practices (Chapter 8) descend from a well-developed lineage: Beck's test-driven
development (Beck 2002) and its small-step, red-green rhythm; Fowler's *Refactoring*
(Fowler 1999) and its behavior-preserving micro-steps; Feathers (2004) on getting
legacy code under a test harness before changing it; and the continuous-integration
tradition (popularized by Fowler and others) whose "keep the build green" ethos the
ratchet (Chapter 12) extends from a binary to a distribution. What is new is the
*reason*: these disciplines were designed to manage human fallibility and team
coordination; I am applying them to manage a collaborator's total inter-session
amnesia, where a green checkpoint is not just good hygiene but the only safe resume
point.

**Decision capture.** The design-fragment / slice-plan separation and the per-fact
memories are close kin to Architecture Decision Records (Nygard 2011) and, further
back, to Knuth's literate programming (Knuth 1984), which insisted that the *why*
travel with the code. ADRs capture the rationale of a decision at the moment it is
made; my memories do the same, with the added twist that the intended reader is a
model that will otherwise reconstruct — or confabulate — the rationale from scratch.

**Specification gaming, Goodhart, and the proxy problem.** Chapter 11 (the agent
optimizes the proxy) and Chapter 12 (ratchets over targets) are the AI-safety
literature's *specification gaming* and *reward hacking* seen from the engineering
side. Amodei et al., "Concrete Problems in AI Safety" (2016), named reward hacking
and scalable oversight as core problems; Krakovna et al. (DeepMind, 2020) catalogued
specification-gaming examples where an optimizer satisfies the letter of an
objective and violates its spirit. Goodhart's law — in Strathern's (1997) crisp
formulation, "when a measure becomes a target, it ceases to be a good measure" — is
the reason a fixed latency budget invites satisficing and a ratchet does not. That a
capable optimizer given an imperfect proxy will exploit the gap is not a quirk of my
setup; it is the field's central finding, and the higher court and the ratchet are
my project-level defenses against it.

**Constitutions and legible reasoning.** Calling `CLAUDE.md` a "constitution" is a
deliberate nod to Constitutional AI (Bai et al., Anthropic, 2022), which aligns a
model's behavior to a written set of principles — but the analogy needs a sharp
distinction. Constitutional AI operates at *training* time, on the model's weights,
to shape its dispositions in general; my constitution operates at *project* time, in
context, to align the *work* on one codebase. They are complementary layers: a
model made broadly cooperative by the former still needs the latter to know that
*this* project forbids kind-branching. The heuristic-mapping discipline (Chapter 6),
which forces reasoning to be stated against named principles, is in the spirit of
chain-of-thought prompting (Wei et al. 2022) making reasoning explicit — used here
not to improve the reasoning but to make it *auditable*. And the multi-agent
adversarial verification I anticipate in Part IV is the practical descendant of
debate-style scalable oversight (Irving et al., "AI Safety via Debate," 2018) and of
learning from human preferences (Christiano et al. 2017).

**Context, retrieval, and the limits of scale.** My claim that "more context is not
more attention" (Chapter 15) rests on real findings: retrieval-augmented generation
(Lewis et al. 2020) exists precisely because feeding everything in is neither
possible nor sufficient, and "Lost in the Middle" (Liu et al. 2023) showed that
models attend unevenly across long contexts, degrading on information buried in the
middle. The authoritative-subset idea is a hand-built retrieval-and-salience layer,
and the literature suggests salience will remain a problem that raw window growth
does not solve. Sculley et al., "Hidden Technical Debt in Machine Learning Systems"
(2015), is the reminder that the maintainable substrate around a model is where the
long-term cost lives — my knowledge base is that substrate, with its own upkeep.

**Agent practice and benchmarks.** The concrete practice sits atop the agent-
architecture line — ReAct (Yao et al. 2022) and the broader reasoning-plus-acting
and tool-use work — and the "easy case vs hard case" framing of Chapter 1 is
sharpened by benchmarks like SWE-bench (Jimenez et al. 2023), which measure agents
on real repository issues and reveal how much harder grounded, interdependent change
is than isolated generation. The most visible alternative to this document's
human-coupled method is the fully *autonomous loop* — the "Ralph" technique
(Huntley), Reflexion-style self-critique (Shinn et al. 2023), Voyager's
skill-accumulation (Wang et al. 2023), and the multi-agent orchestrations; I treat
that family, and where it is similar to and different from the Lattice approach, at
length in Chapter 17, because it belongs to the autonomy-horizon argument of Part IV
rather than to the lineage recounted here. Finally, the specific artifacts I lean on — a checked-in
`CLAUDE.md` / `AGENTS.md`-style constitution, pre-tool hooks, per-fact memory files —
belong to a fast-moving *practitioner* discourse (often labelled "context
engineering") that is, as of this writing, largely ahead of the peer-reviewed
literature. That gap is part of the motivation for writing this down: the direct
study of building large, coherent systems with AI collaborators is young, and most
of what exists is field notes. This document is field notes too — better-instrumented
than most, but field notes, and it should be read as a contribution to that emerging
practice rather than a settled result.

The synthesis, then: I am not proposing new primitives. I am observing that a
capable, eager, forgetful, easy-path-biased collaborator collapses the concerns of
these separate traditions into a single daily practice — Naur's theory-building meets
Goodhart's law meets Beck's small steps meets Brooks's conceptual integrity — and
that the collapse is what is new, because no prior collaborator forced all of them to
the surface at once.

---

## References

Cited works, grouped by the thread they belong to. These are real, verifiable
sources; where I give an approximate year it is the original publication and later
editions exist. Practitioner conventions (checked-in agent constitutions, context
engineering) are deliberately not pinned to a single citation, because as of this
writing that discourse lives in evolving documentation and field notes rather than a
canonical text.

**Complexity, conceptual integrity, modularity**
- Brooks, F. P. *The Mythical Man-Month: Essays on Software Engineering.*
  Addison-Wesley, 1975.
- Brooks, F. P. "No Silver Bullet — Essence and Accidents of Software Engineering."
  *IEEE Computer*, 1986.
- Parnas, D. L. "On the Criteria To Be Used in Decomposing Systems into Modules."
  *Communications of the ACM*, 1972.
- Conway, M. E. "How Do Committees Invent?" *Datamation*, 1968. (Conway's Law.)
- Moseley, B., and Marks, P. "Out of the Tar Pit." 2006.

**Programs as theories; distributed cognition**
- Naur, P. "Programming as Theory Building." *Microprocessing and Microprogramming*,
  1985.
- Hutchins, E. *Cognition in the Wild.* MIT Press, 1995.
- Knuth, D. E. "Literate Programming." *The Computer Journal*, 1984.

**Small, safe steps; testing; continuous integration**
- Beck, K. *Test-Driven Development: By Example.* Addison-Wesley, 2002.
- Fowler, M. *Refactoring: Improving the Design of Existing Code.* Addison-Wesley,
  1999 (2nd ed. 2018).
- Feathers, M. *Working Effectively with Legacy Code.* Prentice Hall, 2004.
- Fowler, M. "Continuous Integration." (essay, 2000s; on the always-green build.)

**Decision capture**
- Nygard, M. "Documenting Architecture Decisions." 2011. (Architecture Decision
  Records.)

**Specification gaming, Goodhart, AI safety / alignment**
- Amodei, D., Olah, C., Steinhardt, J., Christiano, P., Schulman, J., and Mané, D.
  "Concrete Problems in AI Safety." 2016.
- Krakovna, V., et al. "Specification Gaming: The Flip Side of AI Ingenuity."
  DeepMind, 2020 (and the accompanying specification-gaming examples list).
- Strathern, M. "'Improving Ratings': Audit in the British University System."
  *European Review*, 1997. (The popular "when a measure becomes a target…"
  formulation of Goodhart's law; Goodhart, C., 1975, for the original.)
- Christiano, P., et al. "Deep Reinforcement Learning from Human Preferences." 2017.
- Irving, G., Christiano, P., and Amodei, D. "AI Safety via Debate." 2018.

**Constitutions, legible reasoning, oversight**
- Bai, Y., et al. "Constitutional AI: Harmlessness from AI Feedback." Anthropic,
  2022.
- Wei, J., et al. "Chain-of-Thought Prompting Elicits Reasoning in Large Language
  Models." 2022.

**Context, retrieval, and the limits of scale**
- Lewis, P., et al. "Retrieval-Augmented Generation for Knowledge-Intensive NLP
  Tasks." 2020.
- Liu, N. F., et al. "Lost in the Middle: How Language Models Use Long Contexts."
  *TACL*, 2023.
- Sculley, D., et al. "Hidden Technical Debt in Machine Learning Systems." *NeurIPS*,
  2015.

**Agent architectures and benchmarks**
- Yao, S., et al. "ReAct: Synergizing Reasoning and Acting in Language Models."
  2022.
- Jimenez, C. E., et al. "SWE-bench: Can Language Models Resolve Real-World GitHub
  Issues?" 2023.

**Autonomous loops (single- and multi-agent)**
- Shinn, N., et al. "Reflexion: Language Agents with Verbal Reinforcement Learning."
  2023.
- Wang, G., et al. "Voyager: An Open-Ended Embodied Agent with Large Language
  Models." 2023. (Skill-library accumulation — an externalized-memory analogue.)
- The "Ralph" technique (an infinite-loop, single-prompt autonomous coding pattern,
  named for Ralph Wiggum) and the AutoGPT / BabyAGI goal-decomposing-loop lineage are
  practitioner conventions rather than peer-reviewed work; the Ralph framing is
  associated with Geoffrey Huntley's writing on agentic coding. As with the
  context-engineering discourse, these live in evolving field notes, and I cite them as
  the practice they are.
