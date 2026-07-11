# 🧠 Hydragent Core Orchestrator

The Core Orchestrator is Hydragent's decision engine. It is not the gateway, not the memory store, and not the vault. It sits in the middle and turns normalized work into safe, auditable runs while keeping Hydragent's identity intact.

This design borrows the useful parts of the reference systems without copying their identity:

- OpenClaw: always-on gateway behavior, heartbeat wakeups, and session-safe delivery.
- Hermes: persistent profiles, skill growth, and cleanup/curation after real work.
- OpenCode: explicit Plan/Build separation and ordered permission rules.

Hydragent keeps its own nouns: desk, draft paper, pages, books, shelves, Graphify, dreaming, and the Core Orchestrator.

---

## 1. Orchestrator Job

The orchestrator owns one thing: turn an eligible draft into a durable, policy-safe run.

It must:

1. Admit normalized input from a gateway or internal wake event.
2. Bind the work to a stable Hydragent identity.
3. Decide whether the work stays as draft paper, becomes an active run, or waits.
4. Choose a model through the Model Council.
5. Apply the Security Vault before any risky action.
6. Run tools, collect results, and stream progress.
7. Persist the transcript and outcome into the memory system.
8. Trigger dreaming when the run has enough value to compact.
9. Learn reusable skill patterns when the same procedure keeps succeeding.

The orchestrator should be boring on purpose. Boring means deterministic, auditable, and resumable.

---

## 2. The Hydragent Control Loop

Hydragent follows a ReAct-style loop, but the actual mechanism is tighter than generic "think, act, repeat" language.

```text
normalize input
  -> resolve source identity
  -> admit draft paper
  -> classify intent
  -> choose lane
  -> apply policy
  -> enter active run
  -> model turn
  -> optional tool call
  -> settle result
  -> update durable history
  -> optionally compact into pages/books/shelves
```

The loop is not a freeform chat spiral. It is a bounded run with checkpoints.

### What the checkpoints do

- Admit: the desk records the work before execution starts.
- Classify: the orchestrator decides whether the work is conversational, tool-bearing, scheduled, risky, or resumable.
- Execute: the active run owns turn sequencing and tool settlement.
- Settle: the run writes durable output before another turn starts.
- Compact: dreaming later converts useful history into pages and graph links.

---

## 3. Two Primary Modes

Hydragent only needs two top-level runtime modes.

### Plan Mode

Plan Mode is read-only analysis.

It may:

- read Hydragent files and workspace files
- inspect memory pages and graph links
- ask the Model Council for a routing suggestion
- draft a run plan or DAG
- identify approval points before execution

It may not:

- write files
- edit code
- run terminal commands
- launch tools that mutate the host
- bypass the Security Vault

Plan Mode is where the orchestrator decides what should happen. It is not where the change happens.

### Execute Mode

Execute Mode is the action lane.

It may:

- run approved tools
- write to the active desk or workspace
- stream progress back through the gateway
- pause for approval on restricted steps
- resume a suspended run from durable state

If a tool fails, Execute Mode does not invent a new identity or silently continue. It returns to planning with the failure context attached.

---

## 4. Stable Identity First

Hydragent must key work from the source, not from the process that delivered it.

The orchestrator should treat these as identity inputs:

- platform
- chat or session source
- thread and reply lineage
- user identity
- channel scope
- active profile or workspace lane when present

This is the OpenClaw lesson, kept in Hydragent language: channel adapters can change, but source identity must not drift.

The orchestrator must never let the gateway invent identity, and it must never let a retry create a new logical conversation when the source is the same.

---

## 5. Run Lanes

Hydragent should treat the orchestrator as a lane selector, not a single monolithic prompt.

### A. Direct response lane

For simple questions, the orchestrator may answer directly, record the result, and stop.

### B. Tool lane

For work that needs file, shell, browser, or external actions, the orchestrator opens an active run, applies permissions, and steps through tools one at a time.

### C. Background lane

For scheduled work, recurring checks, heartbeats, and delayed follow-ups, the orchestrator runs a background wake path.

### D. Recovery lane

For failed or interrupted work, the orchestrator resumes from the last durable checkpoint instead of replaying everything blindly.

This is the part Hydragent should borrow from OpenClaw's always-on heartbeat model without losing its own library-shaped workflow.

---

## 6. Heartbeat And Wakes

Hydragent should support proactive wakes so it can do useful work without waiting for a new chat message.

The background wake path should cover:

- scheduled reminders
- recurring maintenance
- retry after transient failure
- inbox or channel polling where enabled
- cleanup triggers after long-running sessions

The orchestrator should coalesce wakes that target the same lane and keep a busy-skip state instead of spawning duplicate runs.

Minimum rule:

- if a wake is already running, queue or merge the next wake
- if the wake is only an acknowledgement, do not surface a noisy visible reply
- if the run is retryable, keep the retry reason separate from user-facing state

This keeps Hydragent proactive without turning the desk into a spammer.

---

## 7. Model Selection

The orchestrator does not hardcode model choice.

It asks the Model Council for the best fit, then resolves that choice through the provider layer.

Practical rules:

- explicit caller intent wins when present
- build or deep execution can prefer the stronger model lane
- summaries, cleanup, and dreaming should prefer cheaper or local models when available
- fallback should be explicit and logged

This follows the OpenCode pattern: a named default exists, but there is a clear fallback path and no magic hidden selector.

---

## 8. Permission And Approval

The Security Vault is the approval brain around the orchestrator.

The orchestrator must ask before it crosses a restricted boundary.

Restricted actions include:

- shell commands that touch the host
- writes outside the active workspace
- network actions that can leak data
- tool calls that expose secrets or private files
- package installation or dependency mutation when policy says no

Approval should follow the ordered permission rules from the council/config side, not a random prompt-time guess.

If a step is denied, the orchestrator must settle that denial durably and continue only if the run still has a valid path.

---

## 9. Tool Settlement

Tools are not just side effects. They are part of the run record.

The orchestrator should persist:

- tool name
- validated input
- sanitized output
- start and stop time
- approval state
- retry state
- linked source identity

That gives Hydragent the audit trail it needs to resume, debug, and learn.

Tool output should be trimmed before it becomes part of the visible transcript when it is too large or too sensitive.

---

## 10. Memory And Dreaming

The orchestrator should feed the memory system instead of hoarding everything in the live run.

When a run closes, the orchestrator should decide:

- what stays as draft paper
- what becomes a page
- what needs to join or update a book
- what should link into a shelf or Graphify edge
- what should be compacted during dreaming

Dreaming is not a second conversation. It is the cleanup and distillation pass that turns repeated work into stable Hydragent knowledge.

The practical flow is:

1. Keep the live transcript short.
2. Save durable facts and actions.
3. Compact repeated patterns into pages.
4. Update USER.md and SOUL.md when behavior changes.
5. Generate or update reusable skills when the same procedure keeps winning.

---

## 11. Skill Growth

Hermes gets the important lesson here: useful work should compound.

When Hydragent sees the same successful procedure more than once, the orchestrator should consider a skill candidate.

Promotion path:

1. Detect a repeated successful pattern.
2. Extract the stable steps and parameters.
3. Check for overlap with an existing skill.
4. Write or update `SKILL.md` in the skill engine.
5. Verify the skill on a narrow example.
6. Load it only when it is actually useful.

Skills should stay optional, inspectable, and bounded. The core orchestrator should route to them, not absorb them.

---

## 12. Failure Rules

The orchestrator should fail in predictable ways.

It must:

- preserve the last good checkpoint
- surface tool errors as typed failures
- retry only when the failure is retryable
- stop when policy is unclear
- avoid silently dropping work
- avoid duplicating side effects after a restart

If the system cannot prove the run is still safe, it should pause and ask instead of guessing.

---

## 13. Hydragent Identity Rules

The orchestrator must still feel like Hydragent.

- use desk, draft paper, pages, books, shelves, Graphify, and dreaming
- keep the gateway outside the brain
- keep the vault outside the prompt
- keep the Model Council advisory, not dominant
- keep execution resumable and auditable
- keep background work as a first-class lane
- keep learning as curation, not self-mythology

Hydragent can borrow OpenClaw's gateway discipline, Hermes' self-improving loop, and OpenCode's permission structure, but the system should still read as Hydragent from the first run to the last compacted page.

---

## 14. Suggested State Machine

```text
inbound event
  -> normalize source identity
  -> write draft paper
  -> classify lane
  -> choose model
  -> check policy
  -> if approval needed, pause
  -> enter active run
  -> model turn
  -> optional tool call
  -> settle result
  -> update durable history
  -> if background wake, coalesce or retry
  -> if valuable, compact into pages/books/shelves
  -> if repeated, promote a skill candidate
```

---

## 15. Practical Defaults

- Default to deny when the boundary is unclear.
- Default to the cheapest capable model when the task is simple.
- Default to durable checkpoints before compaction.
- Default to background wake coalescing instead of duplicate runs.
- Default to reusable skills only after a pattern proves itself.
- Default to Hydragent vocabulary, even when the design borrows from outside systems.
