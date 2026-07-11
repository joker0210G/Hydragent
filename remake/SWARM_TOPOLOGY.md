# Hydragent Swarm Topology

Hydragent uses a star-shaped swarm, but the real mechanism is not just "manager + interns". It is a Hydragent-native control plane that admits work, isolates workers, watches liveness, and verifies output without losing the desk, draft paper, pages, books, shelves, and Graphify identity that define the rest of the system.

The design borrows the useful parts of OpenClaw, Hermes, and OpenCode:

- OpenClaw: one control plane, isolated workspaces, multi-channel routing, and a thin gateway boundary.
- Hermes: parallel subagents, skills that grow over time, and long-lived memory.
- OpenCode: durable admission separate from execution, explicit interruption, and stale work cleanup.

## 1. Control Plane First

The swarm manager owns routing and policy. Workers never decide where work goes.

```
                       ┌──────────────────────┐
                       │  Hydragent Manager   │
                       │  (control plane)     │
                       └─┬──────────────────┬─┘
                         │                  │
            ┌────────────┘                  └────────────┐
            ▼                                             ▼
   ┌─────────────────────┐                     ┌─────────────────────┐
   │ Hydragent Builder   │                     │ Hydragent Verifier  │
   │  isolated worker    │                     │  isolated worker    │
   └─────────────────────┘                     └─────────────────────┘
```

Rules:

- No gossip: workers only speak to the manager.
- No shared desk: each worker gets an isolated workspace and its own task boundary.
- No direct mutation outside assignment: a worker can only touch the files or artifacts the manager assigned.
- No hidden ownership: every task keeps an explicit owner, workspace, and retry count.

## 2. Hydragent Work Units

Every task starts as desk work before it becomes execution.

1. A source event lands on the Hydragent desk.
2. The manager writes a draft paper entry for the task.
3. The manager assigns the task to one or more workers.
4. Workers execute inside their own workspace.
5. The manager collects artifacts, verdicts, and logs.
6. Finished work becomes a page, then later a book, shelf entry, or Graphify edge if it is worth keeping.

This keeps the system Hydragent-first: delivery is not execution, and execution is not memory.

## 3. Worker Roles

Use the smallest useful set of worker types:

- Builder: writes or edits code, docs, configs, or automation.
- Verifier: compiles, tests, and checks the result against the task contract.
- Scout: reads source material, extracts relevant patterns, and prepares a short brief.
- Scribe: compresses durable results into pages, books, shelves, and graph links.

The manager can spin up multiple workers for one task, but each worker still gets one lane and one owner.

## 4. Liveness and Zombie Control

Hydragent treats worker silence as a control failure, not as a recoverable mystery.

- Heartbeat interval: 30 seconds.
- Zombie threshold: 90 seconds without a heartbeat.
- Recovery action: terminate the silent process, mark the worker stale, and requeue the task on a fresh worker.
- Preservation rule: do not drop the draft paper entry, the logs, or the last known workspace state.

The manager should prefer explicit interruption over waiting forever. If a worker is still alive but stuck, the manager interrupts first; if the worker stays silent, it is reaped.

## 5. Verification Gate

No worker output is accepted until it passes the verifier lane.

- Code changes must compile or typecheck where the repo expects it.
- Relevant tests run inside the sandbox or the narrowest safe execution boundary available.
- The verifier returns structured logs, not just a yes/no result.
- The manager allows at most 3 correction loops per task.

If the third attempt still fails, the manager stops retrying and surfaces the failure instead of churning the same work.

## 6. Hydragent Identity Rules

The swarm mechanism must keep Hydragent recognizable.

- Use desk, draft paper, pages, books, shelves, and Graphify in user-facing language.
- Treat dreaming as the cleanup and compaction phase, not as a worker role.
- Keep the core orchestrator as the brain and the swarm as helper hands.
- Preserve source origin identity from the incoming channel or workspace so work never becomes anonymous.

That means the swarm can borrow OpenClaw-like routing and Hermes-like growth, but the product language stays Hydragent.

## 7. Suggested State Machine

```text
queued
  -> admitted on desk
  -> draft paper written
  -> worker assigned
  -> running
  -> heartbeat ok
  -> verified
  -> archived as page/book/shelf or graph edge

running
  -> interrupted
  -> stale
  -> zombie reaped
  -> reassigned

verified
  -> merged into durable history
  -> compacted during dreaming when needed
```

## 8. Practical Defaults

- One manager, many workers.
- Prefer narrow specialists over one giant worker.
- Keep one worker per workspace unless the task explicitly requires a shared sandbox.
- Route code writing and verification separately when possible.
- Split long jobs into short, reviewable slices.

## 9. What This Mechanism Is Not

- Not a gossip mesh.
- Not a peer-to-peer swarm.
- Not a shared mutable pile of prompts.
- Not a replacement for memory or dreaming.

It is a controlled Hydragent work topology that borrows the good parts of the reference systems without copying their product identity.
