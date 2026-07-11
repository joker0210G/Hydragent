# Model Provider Architecture Plan

## Goal

Adopt a VS Code-style model-provider model in Hydragent so providers and individual models can be configured, discovered, and routed independently instead of relying on a single global brain configuration.

## Why this matters

The current implementation is functional but relatively coarse:

- one live-brain provider is selected globally through environment variables such as `BRAIN_BASE`, `BRAIN_KEY`, `BRAIN_MODEL`, and `BRAIN_FALLBACKS`
- provider selection is hard-coded in the core bootstrap path
- the model council can choose a model for a task, but it does not own provider-specific configuration or capability metadata
- custom endpoints are supported, but not modeled as first-class provider definitions with per-model settings

A VS Code-style approach would make Hydragent more flexible for:

- bringing your own key for new providers
- adding custom endpoints and self-hosted backends
- configuring different models for different tasks or roles
- exposing model capability metadata such as tool calling, vision, reasoning, context size, and headers
- making provider/model customization easier to maintain over time

## Before vs. After

### Before: one big switchboard

Today, Hydragent mostly works like this:

- one main brain connection is selected from the environment
- that connection points to one provider such as OpenRouter, Ollama, or a custom OpenAI-compatible endpoint
- the app then uses that choice for the whole system

This is simple, but it means the app is a bit like using one single remote control for every device in your house. It works, but it is not very flexible.

### After: a smart model marketplace

With the proposed design, Hydragent would work more like a menu of choices:

- providers become reusable definitions
- each provider can have multiple models
- models can have their own settings and abilities
- the system can choose the right model for the right job

This is more like having a toolbox where each tool is labeled and ready for a specific task.

### Simple version for a 15-year-old

Right now, Hydragent is a bit like having one backpack with one set of stuff for every situation. The new idea is more like having separate bags for school, sports, and travel, so you can grab the right one quickly.

In other words:

- before: one general setup for everything
- after: many specific setups that can be mixed and matched

That makes it easier to use local models, custom APIs, cheaper models, stronger models, and special models for coding, planning, or research.

---

## Key takeaways from the VS Code language-model documentation

The VS Code model system is built around a few strong ideas that are directly useful for Hydragent:

1. Model picker and provider separation
   - Users can switch models from a centralized picker.
   - Models are grouped by provider and can be shown/hidden independently.

2. Provider and model-level customization
   - Providers can be added from built-in providers, extensions, or custom endpoints.
   - Provider configuration can include API keys, endpoint URLs, and provider-specific settings.
   - Models can carry metadata such as display name, identifier, endpoint URL, capability flags, token limits, and request headers.

3. Capabilities matter
   - Models are selected based on capabilities such as tool calling, vision, reasoning, and streaming.
   - This matters for routing and for feature availability.

4. Multiple model roles exist
   - VS Code distinguishes chat, inline chat, inline suggestions, and utility tasks.
   - That pattern is useful for Hydragent too, especially if we want a fast model for lightweight tasks and a stronger model for planning or synthesis.

5. Bring-your-own-key and custom endpoint support
   - VS Code supports built-in providers, extension-based providers, and custom endpoints.
   - This is a good fit for local models, self-hosted OpenAI-compatible APIs, and private enterprise endpoints.

---

## Current Hydragent implementation review

### Core configuration

The current brain configuration is centered in [crates/hydragent-core/src/config.rs](crates/hydragent-core/src/config.rs).

Current behavior:

- `AppConfig` exposes a single `brain_*` configuration surface
- provider detection is based on URL heuristics
- there is no structured registry of providers or models
- provider-specific options are scattered across environment variables and implementation branches

### Provider bootstrap

The main runtime bootstrap in [crates/hydragent-core/src/main.rs](crates/hydragent-core/src/main.rs) constructs the live brain client by branching on provider type:

- `ollama`
- `openrouter`
- everything else as `custom-openai`

This is simple, but it hard-codes the provider decision into runtime startup logic rather than keeping it in a registry.

### Provider implementations

The provider layer is already modular in spirit:

- [crates/hydragent-model/src/openrouter.rs](crates/hydragent-model/src/openrouter.rs)
- [crates/hydragent-model/src/custom_openai.rs](crates/hydragent-model/src/custom_openai.rs)
- [crates/hydragent-model/src/ollama.rs](crates/hydragent-model/src/ollama.rs)

Each provider implements the same `ModelProvider` trait, but the system does not yet model provider capabilities or provider-specific defaults in a reusable way.

### Model routing

The current routing system has two layers:

- [crates/hydragent-model/src/router.rs](crates/hydragent-model/src/router.rs) provides a single primary/fallback router for one provider
- [crates/hydragent-model/src/council.rs](crates/hydragent-model/src/council.rs) and [crates/hydragent-model/src/profiles.rs](crates/hydragent-model/src/profiles.rs) route sub-agents by task type using a YAML-driven model council

This is a good foundation, but it still assumes that a chosen model can be sent to the current provider with minimal metadata.

---

## Proposed architecture

### 1. Introduce a provider registry

Create a central registry that describes providers and models as structured objects.

Suggested types:

- `ProviderDefinition`
  - `id`
  - `kind` (`openrouter`, `custom_openai`, `ollama`, `custom_endpoint`)
  - `display_name`
  - `default_base_url`
  - `auth_mode` (`api_key`, `none`, `custom`)
  - `supports_custom_models`
  - `supports_reasoning`
  - `supports_tools`
  - `supports_vision`
  - `default_headers`

- `ModelDefinition`
  - `id`
  - `provider_id`
  - `name`
  - `aliases`
  - `api_model_id`
  - `tool_calling`
  - `vision`
  - `reasoning`
  - `streaming`
  - `max_input_tokens`
  - `max_output_tokens`
  - `request_headers`
  - `default_params`

This gives Hydragent a VS Code-like registry instead of a single hard-coded provider branch.

### 2. Add a declarative configuration file

Add a new config file such as:

- `config/model_providers.yaml`

This file should define:

- provider presets (OpenRouter, Ollama, custom OpenAI-compatible endpoints)
- named models under each provider
- optional per-model overrides
- defaults for chat, planning, utility, and local/offline use cases

The file should be loaded by the model crate and merged with environment overrides.

### 3. Replace hard-coded provider branching with a factory

The runtime should no longer decide provider type with a string match in one place.

Instead:

- load the registry
- resolve a selected provider/model
- instantiate the correct provider client through a single factory path

This keeps the implementation extensible when new provider types are added.

### 4. Make the model council provider-aware

The existing model council YAML should be upgraded so each profile can reference a model definition from the registry.

Planned evolution:

- keep the current `model_id` field for compatibility
- add optional fields such as:
  - `provider_id`
  - `model_ref`
  - `capability_requirements`
  - `role` or `task_role`

This will let the council route not just by model name, but also by provider capabilities.

### 5. Support per-model and per-provider overrides

Providers should be configurable at two levels:

- provider-level settings
  - base URL
  - API key / auth method
  - default headers
  - timeout / retry policy

- model-level settings
  - endpoint-specific identifier
  - display name
  - reasoning effort support
  - tool/vision capability flags
  - token limits
  - request headers

That will mirror the flexibility described in the VS Code docs.

### 6. Add role-based defaults

Hydragent should support different default models for different activities, similar to VS Code’s chat vs utility model separation.

Suggested roles:

- `chat`
- `planning`
- `coding`
- `research`
- `utility`
- `inline_chat`

This would make it easy to use a fast local model for lightweight utilities and a stronger cloud model for planning or multi-step reasoning.

### 7. Preserve backward compatibility

Existing behavior must continue to work.

Migration strategy:

- keep `BRAIN_BASE`, `BRAIN_KEY`, `BRAIN_MODEL`, `BRAIN_FALLBACKS` as legacy inputs
- map them into a default provider definition automatically
- continue supporting `OPENROUTER_API_KEYS` and `PRIMARY_MODEL` fallback behavior
- allow old `model_council.yaml` entries to continue working while newer entries can reference the registry

### 8. Add user-facing configuration tools

To match the spirit of VS Code:

- expose provider/model inspection through the CLI or REPL
- add commands to list available providers/models
- add commands to set the active provider/model for a role
- eventually support editing the provider registry through a config file or a dedicated command

---

## Recommended implementation phases

### Phase 0 — Design and scaffolding

- define the registry data model
- define the config schema
- add initial tests for schema loading and validation

### Phase 1 — Registry and config loading

- add `config/model_providers.yaml`
- add registry loader in the model crate
- add provider/model definitions for the existing providers

### Phase 2 — Runtime integration

- replace the hard-coded provider bootstrap with a registry-driven factory
- let the live brain and model router resolve providers/models from the registry
- preserve current env-based behavior as a fallback path

### Phase 3 — Council and routing integration

- make `ModelCouncil` profiles reference registry-backed models
- add capability-aware routing where appropriate
- allow role-based or task-based model selection

### Phase 4 — CLI and developer ergonomics

- add commands to print available providers/models
- add commands to switch the active model for a role
- add docs and examples for local/self-hosted providers

### Phase 5 — Advanced customization

- support request headers per model
- support reasoning effort and thinking settings
- support per-provider auth and endpoint overrides
- add support for additional provider backends if needed

---

## Suggested file-level changes

### New or updated files

- `config/model_providers.yaml` — new registry/config file
- `crates/hydragent-model/src/registry.rs` — new registry implementation
- `crates/hydragent-model/src/lib.rs` — export new registry types
- `crates/hydragent-core/src/config.rs` — extend config loading for registry-backed settings
- `crates/hydragent-core/src/main.rs` — use the registry factory for runtime provider creation
- `crates/hydragent-model/src/profiles.rs` — add optional provider/model reference fields
- `crates/hydragent-model/src/council.rs` — make routing aware of provider/model definitions

### Existing provider code to keep

- `crates/hydragent-model/src/openrouter.rs`
- `crates/hydragent-model/src/custom_openai.rs`
- `crates/hydragent-model/src/ollama.rs`

These should remain as concrete implementations behind the registry layer.

---

## Acceptance criteria

The work should be considered successful when:

- providers are described in a declarative config/registry instead of scattered branching logic
- users can add and customize providers/models without changing Rust code
- the model council can route to provider/model combinations based on task and capability
- existing env-based configuration still works
- local and custom endpoints can be configured with minimal friction

## How to check the implementation from your side (Windows CMD)

Because you already have an installed Hydragent and the repository locally, the easiest way to verify this work is to test the repo build first and then compare it with your installed binary.

Use these steps in Command Prompt on Windows. Paste them one block at a time.

### 1. Confirm the installed binary and the repo are both available

```cmd
where hydragent
hydragent --version
cd /d d:\Workspace\Hydragent
git status --short
```

What to expect:
- `where hydragent` should show the installed executable path.
- `hydragent --version` should print the installed version.
- `git status --short` should show the repo state so you can see your local changes.

### 2. Build the local repo version

```cmd
set PATH=C:\Users\DELL-L5420\.cargo\bin;%PATH%
cargo build --release -p hydragent-core
```

What to expect:
- the build finishes successfully with no errors.
- if it fails, stop here and fix the build before moving on.

### 3. Run the model crate tests

```cmd
cargo test -p hydragent-model --lib -- --nocapture
```

What to expect:
- the registry tests should pass.
- if you see failures, the implementation is not ready for user-facing verification yet.

### 4. Verify the local build starts and runs diagnostics

```cmd
target\release\hydragent.exe doctor
```

What to expect:
- the doctor command should print a diagnostic report and exit cleanly.
- it should not crash or fail immediately.

### 5. Verify onboarding still works

If you want to test onboarding without changing your existing config too aggressively, make a backup first:

```cmd
mkdir "%USERPROFILE%\.hydragent" 2>nul
copy "%USERPROFILE%\.hydragent\.env" "%USERPROFILE%\.hydragent\.env.bak" >nul 2>&1
target\release\hydragent.exe onboard --provider ollama --model llama3.1 --non-interactive --no-verify
```

What to expect:
- onboarding completes without crashing.
- a new or updated `.env` file is written.
- the provider is stored as a registry-backed selection rather than falling over in the old bootstrap path.

If you want to restore your previous config afterward, run:

```cmd
copy "%USERPROFILE%\.hydragent\.env.bak" "%USERPROFILE%\.hydragent\.env" /Y >nul
```

### 6. Verify the chat flow with the new runtime

```cmd
target\release\hydragent.exe chat
```

Once the chat prompt appears, try these interactively:

```text
hello
/model
/switch
```

What to expect:
- the chat starts normally.
- `/model` shows the current model selection.
- `/switch` lets you change the active provider/model without editing Rust code.

### 7. Verify the installed version still works as a baseline

This step is useful so you can compare the old installed version with your local build.

```cmd
hydragent test-brain "Reply with exactly the word PONG."
```

What to expect:
- the installed version still responds normally.
- if the repo build behaves differently, you can compare the outputs and see what changed.

### 8. Quick checklist to decide whether the implementation is good enough

You can consider the work verified if all of these are true:

- the local repo build succeeds
- the model tests pass
- `doctor` runs without crashing
- onboarding still works
- chat starts and accepts commands
- provider/model switching is possible from the runtime

If all of those pass, the new provider registry is behaving like a real user-facing feature rather than just a code-only change.

## How this connects to core Hydragent flows

This work is not only about the model layer in isolation. It should connect directly to the main user workflows:

- onboarding: the first-run setup should still be smooth and should let users choose a provider/model
- CLI chat: the main interaction loop should use the selected provider/model without surprises
- REPL and runtime switching: users should be able to change provider/model settings from the interactive experience
- model council routing: task-specific routing should use the registry-backed models instead of only a single global provider choice
- future agent roles: planning, research, coding, and utility tasks should be able to use different defaults cleanly

In other words, the implementation should feel like a better version of the same experience, not a separate hidden system.

---

## Recommendation

The best path is to introduce the registry layer first and keep the current provider implementations intact. That gives Hydragent the flexibility of VS Code’s model system without forcing a rewrite of the runtime. The registry can later grow into a richer model catalog and configuration surface.
