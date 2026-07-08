# Notes: Memory instrumentation for buzz and berd/goose

Working notes for a proposal, 2026-07-07. Source: code scans of `block/buzz`
and `squareup/berd` + `aaif-goose/goose` (Block's goose fork), plus the YAAML
experiment log. Not polished copy.

## What exists today

### buzz (block/buzz — "hive mind" workspace, Rust relay + Postgres)

Memory = "engrams" (NIP-AE, `docs/nips/NIP-AE.md`): per-(agent, owner)
encrypted key-value Nostr events (`kind:30174`). One `core` record injected
into every new session prompt; all other `mem/<slug>` entries are read only
when the agent itself runs `buzz mem ls/get`, navigating `[[wiki-links]]`
from core. **There is no retrieval system**: no embeddings, no ranking, no
automatic recall of non-core memories.

Metrics collected today:

- **NIP-AM turn metrics** (kind 44200, `crates/buzz-acp/src/usage.rs`):
  per-turn token counts and cost. Usage accounting only — no quality signal.
- **NIP-AO observability** (kind 24200, `crates/buzz-core/src/observer.rs`):
  live session telemetry frames for debugging. Not evaluation.
- **Memory quality: nothing.** No judge, no eval, no recall/usefulness
  metric, no feedback loop (grepped `judge|eval|recall|precision|feedback`).
  The desktop memory graph (`desktop/src/features/agent-memory/`) already
  computes orphans/dangling refs — the only quality-adjacent signal, unused.

Key hook points: `crates/buzz-acp/src/engram_fetch.rs` + `pool.rs`
(~1104-1166) — the single choke point where memory is materialized into a
prompt; `crates/buzz-cli/src/commands/mem.rs` — every agent memory read/write;
the NIP-AM turn-metric event — an existing durable per-turn metrics carrier
that recall fields could ride on; relay events (thread replies, reactions,
review approvals) — downstream outcomes a judge could correlate against.

### berd / goose (squareup/berd app + aaif-goose/goose engine)

Berd is a Tauri app driving an upstream goose binary over ACP; berd itself
has **zero memory code**. Desktop agents get goose's builtin memory MCP
extension (`aaif-goose/crates/goose-mcp/src/memory/mod.rs`, one ~670-line
file): category-based flat markdown files, global + per-project. Retrieval is
"load whole category or `*`"; all global memories are concatenated into the
system prompt at extension startup. No embeddings, no ranking, no selection.

Metrics collected today:

- **On memory: none.** No metrics, evals, or telemetry anywhere near the
  memory extension.
- General telemetry that instrumentation could ride on: engine has an otel
  feature (`crates/goose/src/otel/`), PostHog capture, and a Langfuse tracing
  layer; berd has a schema-typed telemetry chokepoint
  (`berd/src/shared/telemetry/client.ts` → Square Unified Events), currently
  emitting only a handful of app-lifecycle events.
- Session transcripts exist and are accessible: engine-side SQLite
  (`sessions/sessions.db`, `session/session_manager.rs`) and ACP
  `exportSession` from berd — an offline-eval feed analogous to YAAML's
  transcript watching.
- Adjacent precedent: Cash's **kgoose memory store** (cash-server,
  `kgoose-memory-store/` + `kgoose/.../memory/MemoryService.kt`) is the only
  sophisticated internal memory system — LLM extraction from conversations,
  topic tags, ACTIVE/REPLACED/EXPIRED lifecycle, replacement chains — but its
  metrics are ops counters (memories upserted), not quality. Its `Memory`
  proto is a natural schema to borrow.

## YAAML metrics worth porting

Tiered by cost. Tier 0 is pure counting — no LLM, a day or two of work per
system. Tier 1 is where the value was for us. Tier 2 is what made results
trustworthy.

### Tier 0: coverage and volume (no LLM required)

- **Recall rate**: recall runs / eligible turns; turns with any memory
  injected. (Buzz: fraction of sessions with core present; count of
  `buzz mem get` reads per turn. Goose: `retrieve_memories` calls +
  categories/memories returned per session.)
- **Context volume**: memories per injection, chars/tokens injected, p50/p90.
  This was the metric that exposed our worst tradeoffs — several "wins"
  turned out to be pure volume increases. Goose's inject-everything-at-startup
  design makes this the first number to look at: it grows monotonically with
  the store.
- **Memory-count buckets** (0 / 1-2 / 3-5 / >5 per injection).
- **Corpus lifecycle**: active memory count, creation rate, staleness (age
  since last read/write), dead memories (never retrieved after N days),
  orphan rate (buzz already computes this in `buildMemoryGraph.ts` — just
  emit it).
- Where to emit: buzz — extend the NIP-AM turn-metric payload (fields:
  memories_injected, memory_chars, mem_reads, core_present); goose — tracing
  attributes on the two handlers in `memory/mod.rs`, riding otel/Langfuse,
  plus berd Unified Events at the `telemetry/client.ts` chokepoint.

### Tier 1: judged usefulness (the actual quality signal)

- **Per-memory judged score (1-5)**: after the turn/session completes, an
  async LLM judge scores each injected memory against the turn it was
  injected into (pre-injection framing: "would this memory be useful context
  for this turn?"). This is the backbone metric — everything else derives
  from it. Both systems have the transcripts to do this offline/async
  (buzz: relay events; goose: sessions.db / exportSession), so it adds zero
  hot-path latency.
- **Useful-run rate** (runs with ≥1 score ≥4, per eligible turn) and
  **low-memory rate** (share of judged memories scoring ≤2). Our current
  headline pair.
- **Abstention taxonomy**: when nothing is injected, was that clean (nothing
  useful existed) or a missed-useful abstention? Requires a wider diagnostic
  retrieval or judge pass. This mattered enormously: it's the only defense
  against the failure mode where a system looks better by injecting less.
  For goose, the analog of "abstention" is a category not being loaded; for
  buzz, non-core memories never being read.
- **Per-memory health**: score history per memory → repeat offenders get
  demoted/retired. In YAAML this became a ranking input
  (health-action rerank); in buzz/goose v1 it can just be a report the owner
  sees (buzz's MemorySection is the natural surface).
- **The one number to optimize: useful recall per context token.** Not
  average score (inflatable by over-abstaining), not recall volume
  (inflatable by injecting everything).

### Tier 2: evaluation infrastructure (what made the numbers trustworthy)

- **Judge calibration before trusting the judge.** Our first judge had 0.135
  kappa against an independent adjudicator; prompt realignment took it to
  0.865. Every quality number was noise until then. Concretely: label ~100
  cases with a second independent prompt/model, measure agreement, only then
  tune against judge scores.
- **Frozen anchor libraries + replay**: a fixed set of historical turns to
  replay retrieval/injection against, so changes are compared on the same
  sample. Goose's sessions.db and buzz's event log both support this.
- **Paired bootstrap CIs on every delta**; deltas whose interval includes
  zero are "no detectable effect." All five of our post-infrastructure
  technique experiments failed this bar — point deltas would have shipped
  several of them wrongly.
- **Holdout set** never used during iteration; winners confirm there before
  shipping.
- **Ceiling diagnostics before optimizing**: our pool-recall oracle showed
  the binding constraint wasn't where intuition said (we assumed thresholds;
  it was a single suppression rule). For goose, the equivalent first
  diagnostic is "of memories judged useful for a turn, how many were even in
  the loaded categories"; for buzz, "how often did a useful mem/<slug> exist
  that the agent never read."

### Lessons that shaped the metric set (transfer as caveats)

- Average judge score alone is actively misleading — strict policies inflate
  it by dropping useful recall. Always pair precision metrics with
  missed-useful metrics.
- LLM judges are redundancy-blind by default: they credit a memory whose
  content is already visible in the context. We needed a second
  incremental-value instrument to catch this (~56% of one apparent win was
  redundancy artifact). Goose's inject-everything design will hit this
  immediately.
- The biggest production failures were write-side, not read-side: stale
  task-state memories outliving their usefulness, and cross-workstream
  bleed. Metrics should segment by memory age and kind from day one; kgoose's
  ACTIVE/REPLACED/EXPIRED lifecycle is ahead of both buzz and goose here.
- Don't ship on point deltas; don't iterate on strategies whose CI includes
  zero.

## Sequencing sketch (for the proposal)

1. **Tier 0 in both systems** — buzz: extend NIP-AM payload + emit orphan
   metrics; goose: otel/Langfuse attributes in `memory/mod.rs` + berd Unified
   Events. Cheap, no LLM, immediately answers "is memory even being used."
2. **Tier 1 async judge in one system first.** Berd/goose is the easier
   start: single ~670-line extension, transcripts in SQLite, engine already
   has tracing rails. Buzz's encryption means judging must run where
   decryption is allowed (agent harness or owner desktop), which is a real
   design constraint worth calling out in the proposal.
3. **Calibrate the judge before publishing any quality dashboard** (the
   0.135 → 0.865 story is the persuasive anecdote).
4. **Tier 2 replay/CI infra** only once someone wants to *change* retrieval
   behavior based on the numbers — that's when uninstrumented iteration
   starts producing false wins.

## Overlap with the AI DX Analytics workstream (#cycle-2-project-ai-dx-analytics)

Reviewed 2026-07-08. There is an active cross-org metrics effort whose
infrastructure the proposal should ride rather than duplicate. What they
have:

- **Session telemetry pipeline** (the substrate): sq-agents CLI telemetry →
  CDP → Snowflake (`CUSTOMER_DATA.DEVTOOLS.SQUARE_EMPLOYEE_AGENT_SESSION`) →
  dbt marts in `forge-tech-health`
  (`TECH_HEALTH.AI_TELEMETRY.FCT_AI_AGENT_SESSIONS`). Session-grain rows for
  Amp, Claude Code, Codex, Goose, Cursor: cost (actual for goose/amp,
  token×rate-card otherwise), tokens by model, tool-call events, subagent
  rollup. Owned largely by Charlie Croom (client) and Drew Traylor (marts).
- **Local LLM session classification** in the sq-agents CLI, feature-flagged:
  per-session category + success labels computed on the user's machine, only
  labels emitted. Coverage is their sore spot — **Goose is at ~4% coverage**,
  by far the worst agent in their table.
- **Local-eval + anonymized-summary pilot** (Jonathan Guy thread, 51
  replies): evaluate sessions locally, emit only eval results and anonymized
  per-prompt summaries — explicitly to avoid shipping raw transcripts.
  Opt-in pilot planned for cycle 3. This is architecturally identical to our
  async-judge pattern.
- **BuilderBot session evals**: a Prefect job (forge-octo-art-prefect) that
  LLM-evaluates BuilderBot conversations from
  `KGOOSE_BUILDERBOT_CHAT_MESSAGES` (support-thread effectiveness, oncall/PD
  variants in progress).
- **Session ↔ PR authorship attribution** (in flight): joining session
  telemetry to PR outcomes — the other half of the "metrics only from PR on"
  gap.

What this means for the proposal:

1. **No overlap on memory itself — the gap is confirmed.** Nothing in their
   pipeline touches memory: no memory events, no memory fields in the
   session schema, no memory quality signal. The proposal is complementary,
   not competitive.
2. **Ride their rails, don't build parallel ones.** For berd/goose, memory
   exposure counters can be emitted as fields on the existing session
   telemetry (the CLI → CDP → FCT_AI_AGENT_SESSIONS path already carries
   goose sessions), and judged memory scores can piggyback on the local-eval
   pilot mechanism rather than a new pipeline. This turns "new
   instrumentation system" into "new fields + one new local eval."
3. **Their Goose coverage problem is our opening.** The workstream's
   weakest data is exactly the agent our proposal instruments; memory
   telemetry lands as part of fixing goose session telemetry generally.
4. **Their local-eval privacy pattern resolves the buzz encryption
   question.** Judge locally where decryption is allowed, emit only
   scores/anonymized aggregates — the same compromise they've already
   socialized for transcripts.
5. **Our calibration methodology is a contribution to their evals, not just
   ours.** The session classifier's success labels and the BuilderBot
   evaluator are uncalibrated LLM judges; the 0.135→0.865 kappa story and
   the calibration protocol apply directly.
6. **Timing hook**: cycle 3 planning is happening now (doc circulating);
   the local-eval pilot is slated for cycle 3 — a memory-metrics addendum
   to that pilot is a natural, small ask.

## Open questions for the proposal

- Buzz: is memory quality an (agent, owner)-private concern (judge runs
  under owner's key, results visible only to owner) or a platform concern
  (aggregated, anonymized)? Encryption forces this choice early.
- Goose: instrument the fork (`aaif-goose/goose`) or upstream OSS
  (`block/goose`) so the community benefits? The memory extension is
  identical upstream.
- Does kgoose (Cash) want the same judge/metric layer? It has the most
  mature memory model and the least visibility into whether extraction is
  producing value.
- Cost envelope: our judge runs on Haiku; per-turn judging of injected
  memories was cents/day at single-user scale. Fleet scale needs sampling
  (judge N% of turns) — the metrics all work on samples.
