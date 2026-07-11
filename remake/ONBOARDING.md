# Hydragent Onboarding & Setup

Hydragent is set up as a local-first assistant with a real lockbox, a real core, and real diagnostics. The goal is not a clever installer. The goal is a setup flow that is explicit, recoverable, and honest about security from the first command.

This path borrows the useful parts of OpenClaw, Hermes, and OpenCode:

- OpenClaw: setup stays explicit, permissions stay visible, and remote exposure is never hidden behind a convenience wrapper.
- Hermes: setup covers approval, sandboxing, skills, and persistent local state.
- OpenCode: config sources, context sources, and durable runtime state stay separated so the system can be reasoned about later.

## 1. Before You Start

Install these basics first:

- Rust 1.78+ for the core runtime
- Python 3.11+ for local scripts and graph tooling
- Git for updates and version history
- Astral `uv` if available, otherwise the installer will fall back to standard Python environment setup

Hydragent should not assume a particular package manager or shell. The setup path should work on Windows, macOS, Linux, and Termux with the least surprise possible.

## 2. Install Hydragent

Use the platform installer, not a hand-rolled setup sequence.

Windows:

```powershell
.\Hydragent.cmd install
```

macOS, Linux, or Termux:

```bash
./install.sh
```

The installer should do the boring work safely:

- detect the operating system and CPU architecture
- fetch or reuse `uv` when it is available
- create the local Python environment
- install Hydragent dependencies
- copy default config files only when they do not already exist
- add the binary path to the user shell profile when needed
- clean up partial work if installation is interrupted

The installer should prefer clear failure over silent partial setup.

## 3. First Run

After install, start the guided setup:

```bash
hydragent onboard
```

The onboarding flow should be short, ordered, and local.

### Stage A: Vault Setup

Hydragent should ask for the vault unlock method first.

- passphrase or PIN for interactive unlock
- local admin or machine-bound file for unattended startup

This stage should only establish the lockbox. It should not ask the model to handle raw secrets or explain them back in plain text.

### Stage B: Brain Setup

Hydragent should ask which provider or model path to use next.

- local-only mode when the operator wants everything offline
- hosted provider mode when the operator wants remote inference
- direct API-key mode when the operator already has credentials

The setup should record the chosen provider path and keep the secret material inside the vault. The model should only ever see placeholders, not the actual secret.

### Stage C: Memory and Skill Sources

Hydragent should then configure the durable work surfaces:

- the desk for active work
- draft paper for in-flight admissions
- pages, books, and shelves for durable memory
- skill discovery roots for reusable tricks
- Graphify for relationship mapping when enabled

This stage should also decide whether the operator wants bundled skills only, optional skills, or broader discovery sources.

### Stage D: Safety Posture

The final setup step should choose the default safety posture:

- default-deny for risky actions
- sandboxed non-main execution for higher-risk work
- approval prompts for shell, network, and file operations that can touch sensitive data

Hydragent should make the safety posture visible instead of burying it in a hidden config side effect.

## 4. What the Wizard Should Write

The wizard should only write canonical local state:

- config files in Hydragent's own location
- vault metadata, not raw secret values
- selected provider and model settings
- enabled skill discovery sources
- safety defaults and sandbox preferences

It should not write temporary setup chatter into durable memory unless the operator explicitly asks it to keep a note.

## 5. Health Diagnostics

If something looks wrong, run:

```bash
hydragent doctor
```

The doctor check should answer the questions that matter operationally:

- Is the runtime installed correctly?
- Can Python, Rust, and the compiler toolchain be found?
- Can the SQLite-backed core read and write its state?
- Is the vault unlocked enough for the current task?
- Are model/provider settings valid?
- Is the sandbox available for risky execution?
- Are the configured skill sources readable?

The doctor should report failures in plain language and point to the broken layer instead of just saying "setup failed".

## 6. Recovery and Reconfiguration

Hydragent setup should be reversible without being sloppy.

- rerunning onboarding should update the selected settings without destroying unrelated local state
- vault changes should stay local and explicit
- provider changes should not silently rewire unrelated memory or skill sources
- if setup was interrupted, the next run should pick up cleanly or restart from the last safe point

This keeps the setup flow closer to OpenCode's durable state discipline than to a one-shot installer script.

## 7. Hydragent Identity Rules

The onboarding experience must still feel like Hydragent.

- use Hydragent nouns: desk, draft paper, pages, books, shelves, Graphify, dreaming, lockbox
- keep the core orchestrator as the brain
- keep setup local-first and explicit
- keep skill loading separate from provider setup
- keep the vault separate from memory

The user should feel they are setting up a Hydragent system, not a generic chatbot wrapper.

## 8. Suggested Setup State Machine

```text
not installed
	-> installer runs
	-> local environment created
	-> vault configured
	-> provider selected
	-> memory and skills discovered
	-> safety posture chosen
	-> doctor passes
	-> ready

ready
	-> rerun onboarding for changes
	-> doctor for checks
	-> settings updated without losing local identity
```

## 9. Practical Defaults

- Default to local-first behavior.
- Default to explicit approvals for risky operations.
- Default to bundled skills only until the operator adds more.
- Default to readable diagnostics over clever summaries.
- Default to preserving existing local state on rerun.
