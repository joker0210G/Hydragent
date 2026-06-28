# Rust Core Crates DOX

## Purpose
This directory contains the core Rust implementation of Hydragent, a high-performance, model-agnostic, and privacy-first AI agent runtime. It includes 16 specialized crates managing memory, planning, tools, security, skills, swarm coordination, and the cryptographic vault.

## Ownership
- Core Runtime & Infrastructure Team (Rust)

## Local Contracts
- Every crate must compile cleanly under standard Rust toolchains.
- Code must adhere to strict safety and security standards, especially within the cryptographic vault (`hydragent-vault`) and WASM sandbox (`hydragent-sandbox`).
- Do not introduce unsafe Rust unless absolutely necessary and thoroughly documented/audited.
- All new features must include corresponding unit or integration tests.

## Work Guidance
- Use the event bus (`hydragent-bus`) for communication between components.
- Keep dependency footprints small to maintain fast compilation and runtime efficiency.
- Follow existing patterns for error handling via `thiserror` or `anyhow`.

## Verification
- Run all unit and integration tests:
  ```bash
  cargo test
  ```
- Run benchmarks to verify performance:
  ```bash
  cargo bench
  ```

## Child DOX Index
- None (all crates are managed under this unified Rust Core DOX contract).
