# Release Notes — v0.5.0

**Hydragent v0.5.0 — "The Swarm"**
Released: 2026-06-13

This is the largest release in hydragent's history. It turns the single-agent
runtime of v0.4 into a coordinated swarm of specialist sub-agents, with
declarative task planning, role-based model routing, file-based inter-agent
messaging, autonomous failure recovery, and operator-facing diagnostics.

---

## What's new in 30 seconds

- You can now describe a multi-step task as a **DAG** (Directed Acyclic
  Graph) of typed nodes, and hydragent will execute the right nodes at the
  right time, in parallel where the graph allows.
- Each sub-task is routed through a **Model Council** that picks the
  best-fit LLM from a pool of 23 profiles based on the task role
  (planner, build, explore, scout, review, general).
- Sub-agents can send **asynchronous mail** to each other through a
  file-backed mailbox, with per-recipient isolation and sequence
  numbers.
- When a sub-agent fails, a **self-healing re-planner** picks one of
  four recovery strategies (Retry, Reroute to a fallback model,
  Decompose into a simpler sub-task, or Escalate) and the engine
  picks the node back up on the next ready-queue tick.
- A **Supervisor** synthesises the outputs of all sub-agents into a
  single final answer.
- A new **`swarm_status` CLI** lets you inspect a saved DAG or
  execution report as ASCII from your terminal.
- A new on-disk **Wiki** gives the swarm a shared Markdown knowledge
  base for cross-run notes.

---

## For users

### The DAG planner

Build a `DagSpec` programmatically:

```rust
use hydragent_planner::dag::*;
use hydragent_planner::decomposer::decompose;

let spec = decompose("Plan a 3-day trip to Tokyo, including flights, hotel, and 2 activities per day.").unwrap();
println!("{}", hydragent_planner::ascii::print_dag(&spec));
```

The decomposer will inspect the prompt, classify it as complex, and
produce a multi-node DAG with edges encoding dependencies. You can also
construct a `DagSpec` by hand:

```rust
use hydragent_planner::dag::*;

let a = DagNode { id: "A".into(), name: "research".into(), /* ... */ };
let b = DagNode { id: "B".into(), name: "summarise".into(), /* ... */ };

let spec = DagSpec {
    swarm_id: "demo".into(),
    page_id: "demo".into(),
    original_task: "research and summarise a topic".into(),
    nodes: vec![a, b],
    edges: vec![DagEdge { from: "A".into(), to: "B".into(), label: None }],
    created_at: 0,
};
```

### The execution engine

```rust
use hydragent_planner::dag_execution::DagExecutionEngine;

let engine = DagExecutionEngine::new(spawner, 4); // max 4 concurrent
let outcome = engine.run_with_outcome(spec, None).await?;
let report = outcome.into_report();
println!("completed={} failed={} wall_ms={}",
    report.completed, report.failed, report.total_execution_ms);
```

The engine respects the per-spec `max_concurrent` cap and supports
cancellation via `tokio_util::sync::CancellationToken`.

### The status CLI

```cmd
# Render a saved DagSpec
swarm_status --from-spec ./data/swarm/s-42/dag.json

# Render a saved ExecutionReport
swarm_status --from-report ./data/swarm/s-42/report.json

# One-line summary, log-friendly
swarm_status --from-report ./data/swarm/s-42/report.json --one-line

# Pipe a spec on stdin
cat dag.json | swarm_status --stdin-spec
```

### The wiki

```rust
use hydragent_planner::wiki::Wiki;

let wiki = Wiki::open("./data/wiki").unwrap();
wiki.save("phase5-architecture", "# Architecture\n\nThe swarm...").unwrap();
let content = wiki.load("phase5-architecture").unwrap();
let hits = wiki.search("swarm").unwrap();
```

Each topic is a single Markdown file in the wiki root. Sanitisation
turns any caller-supplied topic name into a safe `[a-z0-9-_]+`
filename.

### The self-healing re-planner

```rust
use hydragent_planner::replan::SelfHealingReplanner;

let planner = SelfHealingReplanner::new(3, vec!["anthropic/claude-3.5-sonnet".into()]);
let outcome = planner.decide_and_apply(&mut spec, &failure_info);
// or, for deterministic recovery in tests:
let outcome = planner.force_strategy(&mut spec, &failure_info, "reroute");
```

Four strategies, four uses:

| Strategy   | When                                      |
|------------|-------------------------------------------|
| `Retry`    | Transient failure (rate limit, timeout)   |
| `Reroute`  | Model returned a bad answer               |
| `Decompose`| Task was too big for the role             |
| `Escalate` | All retries and routes exhausted          |

---

## For operators

### Diagnostic surface

- `swarm_status --from-report <path>` — ASCII post-mortem of any run.
- `swarm_status --one-line --from-report <path>` — log-shippable summary.
- The `hydragent_planner::ascii` module is also public API, so you
  can wire it into your own dashboard / log pipeline.

### Test count

138 tests in the Phase 5 surface (planner + swarm), all green. The
full repo (including Phases 1–4) is over 300 tests.

### Stress test

`tests/stress_test_phase5.py` runs the full planner + swarm cargo
test suites and exercises the `swarm_status` CLI. Use it as a
release-gate before tagging a new version.

---

## Known limitations

- The decomposer is a rule-based classifier; it does not call an LLM.
  Tasks that fall outside the classification rules will be split into
  a single-node DAG.
- The `swarm_status` CLI does not watch the file system — it reads
  once and exits. Wrap it in your shell's `watch` if you need
  live-updating dashboards.
- The wiki does not currently support locking for multi-writer
  scenarios. A single-process bus of writers is fine; concurrent
  writers from multiple processes may lose updates.

---

## Upgrading

```toml
# Cargo.toml
[dependencies]
hydragent-planner = "0.5"
hydragent-swarm   = "0.5"
```

No breaking changes from 0.4.x. The new modules are additive; the
existing single-agent code paths in `hydragent-core` are unchanged.

See [`CHANGELOG.md`](CHANGELOG.md) for the full list of changes,
including the `hydragent-model` Model Council additions and the
`hydragent-types` `SubAgentRole` extension.
