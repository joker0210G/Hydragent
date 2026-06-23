# User Profile
- Name: User
- Role: Software Engineer & Technical Operator
- Preferred Tone: Professional, direct, and technically rigorous
- Language & Locale: English (Universal)
- Key Constraints: Absolute precision, strict formatting compliance, zero fluff

# Style & Communication Habits
- Terse, imperative, lowercase commands with no pleasantries, punctuation, or capitalization. Often uses ALL CAPS (e.g., 'EXACTLY', 'RIGHT NOW') for emphasis.
- Direct, explicit constraints: exact output words/phrases in quotes, specific word counts, 'and nothing else', 'No more.', em-dashes for emphasis.
- Tool invocations name exact tools and arguments; uses code-block-style key-value pairs (e.g., `content:`, `importance:`) with indented formatting.
- Chains multiple tasks in single sentences using 'and then', 'then', 'finally'; combines actions with follow-up constraints (e.g., summarize in 3 words).
- Wraps exact strings in single/double quotes; uses backticks for identifiers; kebab-case with alphanumeric suffixes for markers (e.g., 'x-session-09ae5c', 'cobalt-lantern-f1cd44').
- Meta-instructions via parentheses: session markers `(marker=x-session-...)`, embedded corrections, clarifications. Uses em-dashes to separate clauses.
- Tests edge cases: path traversal, hidden Unicode, oversized payloads, raw SQL/shell snippets, mixed scripts, zero-width characters.
- British spelling (e.g., 'colour').
- Remembers via `remember:` prefix; references prior context without restating.

# Tool & Output Preferences
- Demands literal, verbatim responses; specific confirmation words (e.g., 'Stored.', 'Done.'); no commentary unless requested.
- Specifies exact tool by name in imperative instructions; structured parameter blocks with `action:`, `rule:`, `memory_id:`, etc.
- Prefers most specific tool for source: `agent_reach.jina_fetch` for URLs, `.youtube`, `.bilibili`, `.github`, `.rss`, `.doctor` for availability. Avoids generic web search fallback unless requested; one retry on network/timeout errors acceptable.
- For security flags: uses ⚠️ prefix, bold bullet 'why' section, 'What I'm happy to do instead' redirect, ### subheaders, horizontal rules, closes with 'What's the real task?'.

# Agent Response Patterns
- Highly structured, professional reports: markdown tables, numbered sections, appendices, executive summaries, classification banners, weighted scoring matrices.
- Uses ✅/❌ for feature support, `> Note` blockquotes, horizontal rules, italicized 'Swarm' attribution, 'Key Open Questions' section.

# Test/Stress Behaviors
- Repeats identical/near-identical templates across turns; appends parenthetical turn/session markers: `(turn N)`, `(wN tN)` format.
- Sends minimal inputs: single characters, empty messages, fragmented words across lines, raw payloads without context.
- Emojis in tests: 🚀🌍✅❓; multilingual code-switching; zero-width spaces; ANSI fragments; pipe-table markdown; fenced code blocks.
- Labels test categories with prefixes like 'Mix:'; uses NATO phonetic alphabet strings; uppercase hyphenated exact-output strings.
- Adversarial patterns: raw SQL injection, shell snippets, `; DROP TABLE users;--`, path traversal.
- Verification tests: structured markers like 'VERIFY FIX 1 RETRY 1'.