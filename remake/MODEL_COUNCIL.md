# Hydragent Model Council

The Model Council is Hydragent's routing brain for models and providers. It does not replace the core orchestrator; it gives the orchestrator a deterministic way to pick the right model for the right task without forcing every sub-agent through the same expensive brain.

This doc now matches the actual code in `crates/hydragent-model/src/council.rs`, `crates/hydragent-model/src/profiles.rs`, `crates/hydragent-model/src/registry.rs`, and `crates/hydragent-swarm/src/spawner.rs`.

## 1. What the Council Actually Does

The council is a read-only routing table plus a selection function.

- It loads profiles from `config/model_council.yaml`.
- It groups profiles by task tag.
- It sorts matches by benchmark score, then by cheaper cost tier.
- It falls back to one primary profile when nothing matches.
- It can resolve a profile through the provider registry when the profile points at a registry-backed model reference.

Hydragent uses the council for sub-agent dispatch. The council is advisory, not magical: an explicit `model_hint` still wins when the caller provides one.

## 2. Provider Surfaces

Hydragent supports these provider shapes in the current implementation:

- OpenRouter for hosted model aggregation
- Ollama for local models
- Custom OpenAI-compatible providers through the registry

The council does not hardcode provider behavior itself. That comes from the provider registry and the model router. The council only chooses a profile, then the runtime resolves it into a concrete provider/model pair when needed.

## 3. Profile Shape

Profiles live in `config/model_council.yaml` and are loaded through `ModelCouncil::load_from_yaml`.

Each profile carries:

- `model_id`: the council-facing identifier
- `provider`: the provider label
- `context_window`: maximum context size
- `cost_per_1k`: conservative output-side cost estimate
- `cost_tier`: free, cheap, standard, premium, or any
- `task_tags`: the task tags this profile is good at
- `benchmark`: optional per-tag scores used as the tie-breaker
- `primary`: exactly one profile must be the safety-net fallback
- `model_ref`: optional registry reference for provider-backed resolution
- `capability_requirements`: optional capabilities that the resolved model must satisfy

That means Hydragent can keep a clean routing name in the council while still resolving to the real provider definition later.

## 4. Routing Rules

The current routing order is simple and intentionally boring.

1. Find profiles whose `task_tags` contain the requested task tag.
2. Filter them by the caller's `CostTier` budget.
3. Sort the survivors by benchmark score, then cheaper tier.
4. If nothing survives, return the cheapest matched profile when the task tag matched but the budget was too tight.
5. If nothing matched at all, return the single `primary` profile.

The selection breadcrumb is preserved in the routing decision so the runtime can log how the choice happened.

### What the code does, not what the sketch promised

- There is no separate generic "capabilities filter" in the council itself yet.
- Budget filtering is real and enforced through `CostTier`.
- Benchmark score is the tie-breaker for profiles that match the task.
- Unknown task tags go to the primary profile.
- Explicit overrides go through `route_explicit` or the spawner's `model_hint` path.

## 5. Native Task Tags

Hydragent's native sub-agent roles currently map into these council tags:

- `Build` -> `code_generation`
- `Explore` -> `research`
- `Plan` -> `planning`
- `Review` -> `review`
- `Scout` -> `summarization`
- `General` -> `general`

That is the real interface between the swarm and the council. The planner can emit richer task structure, but the council currently routes by these task tags.

## 6. Explicit Override Path

The council is not a dictator.

If a sub-agent spec sets `model_hint`, the spawner honors it. If the model hint is unknown to the council, Hydragent warns but still proceeds because the caller is explicitly asking for that model.

If the caller uses `route_explicit`, the council returns an `Explicit` routing decision for audit clarity.

That behavior matters because Hydragent needs both:

- automatic routing for normal work
- caller override for edge cases, debugging, and exact model pinning

## 7. Registry-Backed Resolution

Hydragent already has native support for provider-backed model resolution.

The council can attach a provider registry and resolve `model_ref` entries like `provider_id/model_id` into the actual provider/model pair.

That gives Hydragent a useful split:

- the council decides what kind of model is wanted
- the registry decides what concrete provider definition satisfies it

If a profile declares capability requirements, the registry can reject a resolved model that does not satisfy them.

## 8. Dreaming and Local Fallback

Dreaming should prefer cheap or local options when available.

Hydragent already supports the important part: a `free` primary safety-net and local Ollama profiles in the council config. That means the dream phase can stay cheap without needing a separate magical "night mode" model system.

The practical pattern is:

- local or free profiles for compaction, summarization, and cleanup
- stronger cloud profiles for planning, reasoning, or review when needed
- one primary profile as the always-available fallback

## 9. Hydragent Identity Rules

The council should still read as Hydragent, not as a generic router.

- use Model Council, not a generic model balancer
- keep the librarian imagery if it helps, but do not let it hide the real routing rules
- keep task tags aligned with the swarm and planner vocabulary
- keep the provider registry separate from the council
- keep the primary fallback explicit so the runtime is predictable

## 10. Suggested Routing State Machine

```text
task tagged
    -> council lookup
    -> explicit model_hint? use it
    -> tag matches profiles
    -> budget filter
    -> benchmark ordering
    -> choose best in budget
    -> if none, choose cheapest matched profile
    -> if no match, choose primary
    -> optionally resolve through provider registry
    -> hand concrete model to router
```

## 11. Practical Defaults

- Keep exactly one primary profile.
- Keep free or cheap profiles available for routine work.
- Keep benchmark scores aligned with real observed performance.
- Prefer registry-backed model refs when a provider definition already exists.
- Treat explicit model hints as caller intent, not as an error path.
