# Hydragent Skill Engine

The Skill Engine is Hydragent's book of tricks: a controlled way to discover, load, use, improve, and retire reusable workflows without turning the whole system into one giant prompt pile.

This design borrows the useful parts of OpenCode, Hermes, and OpenClaw:

- OpenCode: skills are discovered from location-scoped sources, loaded intentionally, and gated by permission.
- Hermes: skills can be first-class reusable assets, bundled for baseline UX, installed from a hub, or created by the agent.
- OpenClaw: core stays thin; stronger skills belong in a published or optional layer unless they are truly product-critical.

## 1. What a Skill Is

A skill is a Markdown file with a small header and a clear procedure body.

```yaml
---
name: make_funny_poem
description: Turns boring news into a funny poem.
version: 1.0.0
author: Hydragent
parameters:
  - name: topic
    type: string
    description: The subject of the poem.
tools_required:
  - web_search
---

# Instructions
1. Search the web for the latest updates on {{topic}}.
2. Summarize them in four rhyming lines.
```

Hydragent keeps the file format simple on purpose:

- `SKILL.md` owns the canonical instructions.
- Supporting scripts, references, and templates stay beside the skill, not inside the core.
- The description should stay short and useful, not promotional.
- The skill should say what it needs, not how the whole system works.

## 2. Skill Discovery

Skills are discovered from explicit sources, not from an unbounded global search.

Discovery sources can be:

- local skill roots inside the Hydragent library
- workspace skill folders
- curated shared skill directories
- remote discovery URLs when the operator explicitly allows them

Hydragent should treat discovery as a read-only inventory step. A discovered skill is not active until the manager intentionally loads it for the current task or agent.

The important boundary is this:

- discovery answers "what exists"
- loading answers "what can this agent use"
- invocation answers "what should run now"

## 3. Skill Placement

Skills belong in one of three practical tiers:

- Bundled skills: baseline Hydragent behavior that ships with the product.
- Optional skills: official but not always-on capabilities that can be installed when needed.
- Generated skills: new skills distilled from successful work and saved into the library.

Hydragent should keep the core small and let most reusable tricks live in the skill library instead of hardcoding them into orchestrator logic.

## 4. Permission and Loading

Skill loading is permission-checked and scope-aware.

- The selected agent only sees the skills it is allowed to use.
- Skill bodies stay behind a gated lookup path.
- Available-skill guidance should list names and brief descriptions, not leak unnecessary absolute locations.
- If a skill is disabled or inactive, the loader should not surface it as usable.

This keeps the model-facing surface honest: the model can ask for a skill, but the manager still decides whether the skill is available, permitted, and current.

## 5. Skill Invocation

Invoking a skill should feel like a narrow, explicit handoff.

1. The manager selects the skill.
2. The skill receives only the parameters it declared.
3. The skill uses only the tools it requested and the permissions it has.
4. The result is settled back into the Hydragent workflow as a normal durable outcome.

No skill should silently gain broader authority than it declared. If a skill needs more power, that power should come from an explicit revision, not from hidden expansion.

## 6. Auto-Induction

Hydragent can learn new tricks from successful work, but only when the result is stable enough to reuse.

The induction loop is:

1. Detect a repeated success on a hard task.
2. Generalize the task by replacing one-off details with placeholders.
3. Check for overlap with existing skills before saving anything new.
4. Write the candidate skill into the library.
5. Run a local verification pass before promotion.

What gets generalized:

- concrete URLs become `{{target_url}}`
- file paths become `{{path}}`
- user-specific values become named parameters
- transient model chatter is removed

What does not get generalized:

- Hydragent's own vocabulary
- task boundaries
- permission requirements
- tool requirements

That keeps the new skill reusable without flattening it into a vague template.

## 7. The Curator

The Curator is Hydragent's nightly skill maintenance pass.

It should check:

- usage count
- failure rate
- recent regressions
- overlap with newer skills
- whether the skill still matches current tool and policy boundaries

If a skill has been used often enough to matter but fails too often, the Curator should try the safest repair path first:

1. restore the previous known-good version from history
2. re-run a narrow verification check
3. mark the skill inactive if it still fails

Inactive means the skill stays in the library for reference, but the active loader stops offering it to the model.

## 8. Hydragent Identity Rules

The Skill Engine must sound and behave like Hydragent, not like a generic assistant framework.

- Use Hydragent language: desk, draft paper, pages, books, shelves, and Graphify.
- Treat skills as reusable tricks in the library, not as a second memory system.
- Keep dreaming as the compaction and cleanup phase, not as skill execution.
- Keep the core orchestrator in charge of policy and routing.
- Keep skill files simple enough that humans can inspect them.

The product can borrow the strongest ideas from the reference systems, but the user should still feel they are working inside Hydragent.

## 9. Suggested Skill Lifecycle

```text
discovered
  -> permission checked
  -> loaded for current agent
  -> invoked
  -> verified
  -> kept active

invoked repeatedly
  -> if useful, promoted to bundled or optional skill
  -> if successful patterns repeat, generalized into a generated skill

failing or stale
  -> restored from history
  -> re-verified
  -> marked inactive
```

## 10. Practical Defaults

- Prefer fewer, better skills over many near-duplicates.
- Keep skill descriptions short.
- Keep skill bodies deterministic and task-shaped.
- Let the library grow from real wins, not from guesswork.
- Keep the skill engine thin enough that the rest of Hydragent stays readable.
