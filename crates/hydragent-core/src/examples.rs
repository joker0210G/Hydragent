// crates/hydragent-core/src/examples.rs
//
// `hydragent examples` — print a small catalogue of starter prompts
// a new user can paste straight into `hydragent chat` to verify the
// brain is doing what the docs claim.
//
// Each example is annotated with the tools it'll exercise, so the user
// can pick one that matches the brain they wired up.

pub struct Example {
    pub title: &'static str,
    pub tools: &'static [&'static str],
    pub prompt: &'static str,
    pub note: &'static str,
}

const EXAMPLES: &[Example] = &[
    Example {
        title: "Identity check (zero-tool, fastest possible smoke test)",
        tools: &[],
        prompt: "Reply with exactly: PONG. No other text.",
        note: "Confirms the brain is reachable and the router is wired up. Use this right after `onboard`.",
    },
    Example {
        title: "Plain chat (no tools)",
        tools: &[],
        prompt: "Explain in 3 sentences why Rust's ownership model is unique.",
        note: "Tests stream latency and Markdown rendering. Should be sub-second on a hosted model.",
    },
    Example {
        title: "Calculator via the `echo` tool (sandboxed WASM)",
        tools: &["echo"],
        prompt: "Use the echo tool to repeat the string \"hello from the sandbox\". Then summarise in one sentence.",
        note: "Confirms the WASM sandbox is loaded. If this fails, run `hydragent doctor`.",
    },
    Example {
        title: "Web search",
        tools: &["web_search"],
        prompt: "Use web_search to find the latest release of the Rust programming language, then tell me the version number and release date.",
        note: "Requires SEARXNG_BASE_URL to be set in .env (or a working default).",
    },
    Example {
        title: "Memory write + recall",
        tools: &["memory_store", "memory_search"],
        prompt: "Remember this fact using memory_store: \"the user's favourite colour is indigo\". Then on the next turn, search your memories for it.",
        note: "Two turns: first write, then in a new turn ask \"What is my favourite colour?\" — the model should hit memory_search and find it.",
    },
    Example {
        title: "Multi-step ReAct (use file_read + memory_search)",
        tools: &["file_read", "memory_search"],
        prompt: "Read the file README.md and tell me three things I can learn from it. Then search your memories for any prior notes on this README.",
        note: "Drives the agent through 2+ tool calls; useful to verify MAX_REACT_STEPS is sane (default 10).",
    },
    Example {
        title: "Phase 6 security surface (audit chain)",
        tools: &["audit_query"],
        prompt: "Show me the last 5 audit events using audit_query.",
        note: "Only works after the audit chain has been initialised (happens on first chat).",
    },
    Example {
        title: "Standing orders (SOUL.md / USER.md)",
        tools: &["soul", "user_profile"],
        prompt: "Read my SOUL.md and USER.md using the soul and user_profile tools, and tell me what they say in one line each.",
        note: "Files must exist at config/SOUL.md and config/USER.md (copy from the .example files).",
    },
];

/// Print the catalogue to stdout. `kind` filters by tool requirement.
pub fn print(kind: Option<&str>) {
    println!();
    println!("------------------------------------------------------------------------");
    println!("  🐉 Hydragent — example prompts to try in `hydragent chat`");
    println!("------------------------------------------------------------------------");
    println!();
    let mut n = 0;
    for ex in EXAMPLES {
        if let Some(filter) = kind {
            if !ex.tools.iter().any(|t| t.contains(filter)) {
                continue;
            }
        }
        n += 1;
        println!("  [{}] {}", n, ex.title);
        if !ex.tools.is_empty() {
            println!("      tools : {}", ex.tools.join(", "));
        } else {
            println!("      tools : (none — pure chat)");
        }
        println!("      prompt: {}", ex.prompt);
        if !ex.note.is_empty() {
            println!("      note  : {}", ex.note);
        }
        println!();
    }
    if n == 0 {
        eprintln!("  No examples matched the filter '{}'.", kind.unwrap_or(""));
    } else {
        println!("  Tip: copy any prompt above into `hydragent chat`, or run");
        println!("       `hydragent examples memory` to filter by tool name.");
    }
    println!("------------------------------------------------------------------------");
}
