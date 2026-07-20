// Library crate for hydragent-core.
//
// Exists so downstream crates (notably `hydragent-bench`/`bin/task_bench`)
// can depend on the agent-loop primitives without pulling in the entire
// `bin/hydragent` CLI (which still owns cli_repl, doctor, onboard,
// tui_header, status_bar, examples, markdown_render, logger, session,
// uninstall, update).
//
// The full binary continues to declare its own broader set of
// `pub mod ...;` declarations at `src/main.rs`; the two crates (bin and
// lib) coexist within this single package — they share files via the
// crate boundary, not the file boundary.
//
// Rule of thumb for this lib: keep it MINIMAL. Adding modules here means
// re-deriving any internal `crate::...` paths they reference, and may
// drag in cascading transitive deps. Only add a module if a downstream
// crate genuinely uses it.
//
// Closure of internal `crate::` deps (verified by grep):
//   paths              → none
//   config             → paths
//   strategy           → none
//   swarm_runner       → strategy
//   skill_induction    → none
//   dream              → paths, skill_induction
//   orchestrator       → paths, strategy, swarm_runner, react_loop, dream
//   react_loop         → orchestrator

pub mod config;
pub mod dream;
pub mod orchestrator;
pub mod paths;
pub mod react_loop;
pub mod skill_induction;
pub mod strategy;
pub mod swarm_runner;
