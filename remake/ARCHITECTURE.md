# 🐉 Hydragent Remake Architecture Specification

Hydragent is a single system with four clear layers. The names stay Hydragent-specific so the product identity remains visible from the first message to the final memory page.

This architecture follows the same practical lessons as the reference systems, but keeps the Hydragent vocabulary:

- normalize at the edge
- keep source identity deterministic
- separate draft intake from execution
- compact into pages instead of hoarding raw chat
- connect memory with Graphify-style links

## Quick Navigation

- [CHANNEL_GATEWAYS.md](CHANNEL_GATEWAYS.md) - platform ingress and outbound delivery
- [CORE_ORCHESTRATOR.md](CORE_ORCHESTRATOR.md) - draft intake, runs, tools, and policy
- [MEMORY_SYSTEM.md](MEMORY_SYSTEM.md) - desk, draft paper, pages, books, shelves, Graphify
- [DREAMING_PHASE.md](DREAMING_PHASE.md) - compaction and cleanup
- [SECURITY_VAULT.md](SECURITY_VAULT.md) - secrets, approvals, and sandboxing
- [MODEL_COUNCIL.md](MODEL_COUNCIL.md) - model and provider routing
- [SKILL_ENGINE.md](SKILL_ENGINE.md) - skills and reuse
- [SWARM_TOPOLOGY.md](SWARM_TOPOLOGY.md) - helper agents and task splitting

---

## 1. The 4-Layer Hydragent Body

```
 ┌────────────────────────────────────────────────────────┐
 │ Layer 1: Hydragent Channel Gateways                    │
 │ Normalize platform events into desk-ready envelopes   │
 └──────────────────────────┬─────────────────────────────┘
                            │ normalized events and replies
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │ Layer 2: Hydragent Core Orchestrator                   │
 │ Owns draft intake, runs, tools, policy, and streaming │
 └──────────────────────────┬─────────────────────────────┘
                            │ policy checks and durable state
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │ Layer 3: Hydragent Security Vault                      │
 │ Owns secrets, approvals, sandboxing, and guardrails   │
 └──────────────────────────┬─────────────────────────────┘
                            │ persistent records
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │ Layer 4: Hydragent Diary & Library                     │
 │ Owns Desk, Draft Paper, Pages, Books, Shelves, Graphify│
 └────────────────────────────────────────────────────────┘
```

---

## 2. Layer 1: Hydragent Channel Gateways

The channel gateways are the ears and mouth of Hydragent. They are not the brain.

Their job is to turn Telegram, Discord, Slack, CLI, webhook, and local UI input into one canonical message contract before the core sees it.

### What they own

- platform auth and webhook verification
- mention stripping, thread mapping, and scope mapping
- attachment download into a restricted scratch path
- rate limiting and duplicate suppression
- outbound rendering for each platform's message format

### What they must preserve

- `platform`
- `chat_id`
- `chat_type`
- `thread_id`
- `scope_id`
- `user_id` and `user_id_alt`
- `chat_name`, `chat_topic`, `parent_chat_id`, and `message_id` when available

### Canonical edge contract

Every inbound event becomes a normalized `MessageEvent` with a frozen `SourceOrigin`.

The important rule is simple: Hydragent must key identity from the source discriminators, not from the bot token, socket, or worker process that happened to deliver the event.

---

## 3. Layer 2: Hydragent Core Orchestrator

The core orchestrator is the decision layer. It does not know Telegram from Slack or Discord from CLI. It only knows normalized sources, drafts, pages, tools, and policy.

### Core responsibilities

- admit incoming work into durable draft paper
- promote a draft into an active run when it is eligible
- sequence model turns
- run tool calls with typed input and output
- stream replies and tool progress
- trigger compaction when context pressure grows
- apply policy before model or tool use

### Desk, draft, and execution flow

Hydragent should follow the same Hydragent-native path everywhere:

1. A gateway delivers a normalized event to the desk.
2. The core derives the work lane from the `SourceOrigin`.
3. Draft paper records the incoming prompt before execution starts.
4. The orchestrator promotes the draft into an active run when eligible.
5. Tool work and model turns happen inside that active run.
6. Interruption pauses execution without deleting the durable draft or transcript.

That split matters because it keeps delivery, drafting, and execution independent. A message can be admitted even when execution is paused, drained, or waiting on policy.

### Prompt and execution boundary

Hydragent should keep the same separation that OpenCode uses, but frame it as a desk-to-page path:

- prompt admission becomes durable draft paper
- execution is resumable
- visible history is projected from committed pages and transcripts
- retry and resume are explicit, not implicit

### Context and compaction

The core should treat system context as a stable baseline that can be refreshed at safe boundaries.

That means:

- capture the current system context before a model turn
- keep model-turn selection scoped to the active turn
- rebuild the baseline after compaction
- never let a stale model choice leak across the wrong boundary

When the active request grows too large, Hydragent should compact the active transcript into pages, keep the durable history, and continue from the completed checkpoint instead of silently truncating state.

### Tool execution

Tools belong to the core execution path, but they are still governed by policy and sandbox rules.

The core should:

- decode validated input before tool execution
- authorize the action before touching trusted resources
- persist tool settlement durably
- preserve call and settlement identity across retries
- surface expected failures as typed failures, not crashes

---

## 4. Layer 3: Hydragent Security Vault

The security vault is the shield around Hydragent.

It is responsible for everything that should not be mixed into the normal chat flow:

- API keys and signing secrets
- webhook signatures and shared secrets
- permissions and approvals
- sandbox boundaries for risky operations
- deny-by-default guardrails for unsafe paths

The vault exists so the core can ask, but not guess.

This is where Hydragent keeps the difference between a valid request and a dangerous request.

---

## 5. Layer 4: Hydragent Diary & Library

This is Hydragent's memory path. It is not a generic blob store; it is a tidy library that turns messy chat into durable knowledge.

The memory layer should hold the durable facts that let Hydragent survive restarts and preserve continuity:

- the desk as the active workspace
- draft paper as temporary in-flight chat
- pages as condensed knowledge units
- books as grouped pages
- shelves as grouped books
- graph edges between related memories
- message transcripts and durable history
- token and cost summaries
- custom memory notes such as `USER.md` and `SOUL.md`

The storage model should be simple and library-shaped:

- the desk is the current active workspace
- draft paper stays temporary until it is worth keeping
- pages are the condensed knowledge units
- books group related pages
- shelves group related books
- graph links connect the library into a searchable map

### Graphify and dreaming

Hydragent should use Graphify-style linking during the dreaming cycle:

- compress messy chats into pages
- update or prune `USER.md` and `SOUL.md` when they grow too large
- connect related pages, books, and shelves with edges
- generate a navigable map of what Hydragent knows

That keeps memory useful instead of letting chat logs pile up forever.

---

## 6. Hydragent Runtime Flow

This is the actual path a message follows.

```text
Platform event
  -> Channel gateway normalizes source
  -> Work lane is derived
  -> Draft paper is recorded durably
  -> Policy and permissions are checked
  -> Model turn starts
  -> Tools may run
  -> Streaming reply is emitted back through the gateway
  -> Durable history is updated
  -> Dreaming cycle later compacts into pages, books, shelves, and graph edges
```

The important property is that each step has one owner.

- gateways own platform translation
- the core owns execution and work-lane semantics
- the vault owns trust boundaries
- the diary and library own durable knowledge

### Hydragent desk vs. draft paper

The active workspace is the desk. The current unsaved conversation is draft paper. Once the system decides the material matters, it becomes a page, then joins books and shelves through Graphify links.

---

## 7. Model And Policy Shape

Hydragent should keep the model-council idea, but it should be framed as part of Hydragent, not as a separate identity.

That means:

- the orchestrator selects the model for the task
- provider policy can allow or deny provider use
- model choice is not hardcoded inside a gateway
- provider options stay separate from provider identity

This keeps the system flexible without losing the Hydragent brand or splitting behavior across unrelated layers.

The model council should support the diary/library flow rather than compete with it: the right model helps write summaries, extract facts, and compact memory, while the local graph engine keeps the structure connected.

---

## 8. What Hydragent Must Not Do

To keep the architecture clean, Hydragent should not:

- let the gateway invent identity
- let the core depend on platform-specific quirks
- let secrets leak into prompt context
- let memory become a random dump of chat logs instead of pages, books, and shelves
- let provider policy be bypassed by a plugin or adapter

If a boundary is unclear, the default should be to push the responsibility outward to the smallest layer that actually owns it.

---

## 9. Summary

Hydragent is not just a bot wrapper. It is a structured system with a clear edge, a clear brain, a clear trust boundary, and a clear diary-and-library layer.

The key architecture rule is the same one used in the reference systems: normalize early, route deterministically, execute durably, compact into pages, connect with Graphify, and keep the product identity intact as Hydragent.