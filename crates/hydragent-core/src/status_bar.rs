// crates/hydragent-core/src/status_bar.rs
//
// Bottom statusline inspired by the Kimi-CLI / Hyper CLI family.
//
//   mode → shift+tab · multi-model (kimi-k2.6) → ctrl+p · ████░░░░░░░░░░░░ 25% ctx · ↑411k ↓6.9k · / for commands
//
// The bar is **state-driven**: the REPL owns a `StatusState` and
// calls `render_status_bar(&state)` whenever something changes
// (e.g. after a turn completes, the token counters go up). The
// rendered string is the contract — tests assert on the bytes.
//
// We deliberately keep this module sync + stateless. The REPL
// can choose to redraw the bar in-place (using crossterm cursor
// save/restore), but the *function* itself does not touch a
// terminal. That makes it trivially testable and trivially
// mockable.

use owo_colors::{OwoColorize, Style, Stream::Stdout};

/// What "mode" the REPL is in. Today there's only one — `Normal`
/// — but the Kimi design makes the mode explicit so the user can
/// see at a glance which rules apply. Future expansion: `Plan`
/// (read-only), `Ferment` (background task), `Multi` (multi-model
/// routing). They are all just enum variants that the renderer
/// stringifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Standard interactive mode.
    Normal,
    /// Read-only planning mode (the user can ask questions but
    /// no tool calls fire). Shown on the bar as `plan`.
    Plan,
    /// Background-task mode (the model is running async, the
    /// user is in a different conversation). Shown as `ferment`.
    Ferment,
}

impl Mode {
    /// Short string used on the bar. Lowercase, no spaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Plan => "plan",
            Mode::Ferment => "ferment",
        }
    }
}

/// The full bar state. Cheap to `Clone` (no `String` copies for
/// `mode`) and `Send` so it can live inside an `Arc` shared
/// with the rendering thread.
#[derive(Debug, Clone)]
pub struct StatusState {
    /// Current REPL mode. The bar's leftmost token.
    pub mode: Mode,

    /// Active model name. Shown after the `→ shift+tab` hint
    /// and again in the `→ ctrl+p` hint's parenthesised
    /// "(current model)" slot.
    pub model: String,

    /// True if multi-model routing is enabled (e.g. the Model
    /// Council is active). The Kimi design shows this as
    /// `multi-model (kimi-k2.6)` — a single tag with the active
    /// model in parens.
    pub multi_model: bool,

    /// 0..=100. Rendered as a 20-cell block bar (`████░░░░…`).
    /// `100` is a fully-full context window; we *don't* show
    /// 100% as a hard error — the REPL keeps working, it just
    /// visually flags that the model will start to truncate.
    pub context_pct: u8,

    /// Cumulative prompt tokens consumed this session. The
    /// `↑` arrow in the original design always means
    /// "input/prompt" so the user can read the bar top-to-bottom
    /// as "what went in, what came out".
    pub input_tokens: u64,

    /// Cumulative completion tokens produced this session. The
    /// `↓` arrow always means "output/completion".
    pub output_tokens: u64,

    /// Hint shown in the trailing segment. The Kimi design uses
    /// `phase:plan` here. We mirror that pattern but localise
    /// the leading text so the user knows it's the same `phase`
    /// they see on the left.
    pub slash_hint: String,
}

impl StatusState {
    /// Convenience constructor for the most common case: normal
    /// phase, single model, zero tokens. The REPL calls this at
    /// startup and then mutates fields as the conversation
    /// progresses.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            mode: Mode::Normal,
            model: model.into(),
            multi_model: false,
            context_pct: 0,
            input_tokens: 0,
            output_tokens: 0,
            slash_hint: "/ for commands".to_string(),
        }
    }
}

/// Render the status bar as a single line. We always emit a
/// trailing newline so the caller's `println!` doesn't have to.
///
/// Layout (with the dot as a separator):
///   <phase> → shift+tab · <model_tag> → ctrl+p · <ctx bar> <pct>% ctx · ↑<in> ↓<out> · <slash_hint>
///
/// where `<model_tag>` is either `model (kimi-k2.6)` (single
/// model) or `multi-model (kimi-k2.6)` (multi-model routing).
pub fn render_status_bar(s: &StatusState) -> String {
    let dim = Style::new().dimmed();
    let cyan = Style::new().cyan();
    let bold_cyan = Style::new().bold().cyan();

    // We bind the source strings to local variables first so
    // the `if_supports_color` return value can borrow from a
    // value whose lifetime extends to the end of the function
    // (rather than just the let-statement). Without this, the
    // borrow checker sees the temporary drop too early.
    let phase_text = s.mode.as_str().to_owned();
    let phase_str = phase_text.if_supports_color(Stdout, |p| p.style(bold_cyan));
    let model_tag = if s.multi_model {
        format!("multi-model ({})", s.model)
    } else {
        format!("model ({})", s.model)
    };
    let model_str = model_tag.if_supports_color(Stdout, |m| m.style(cyan));

    // Context bar: 20 cells, filled proportionally to `context_pct`.
    // We colour the filled cells green when usage is low (< 60%),
    // yellow in the warning band (60..85%), and red beyond 85%.
    // The empty cells are always dim grey.
    let bar_str = render_context_bar(s.context_pct);
    let pct = format!("{}% ctx", s.context_pct);

    // Token counters. The arrows are part of the contract: `↑`
    // for input (prompt tokens) and `↓` for output (completion
    // tokens). The numbers are formatted with thousands
    // separators so a 1,234,567 token session doesn't render
    // as an unreadable string of digits.
    let tokens = format!(
        "↑{} ↓{}",
        format_thousands(s.input_tokens),
        format_thousands(s.output_tokens),
    );
    let slash = s.slash_hint.if_supports_color(Stdout, |h| h.style(dim));

    let pct_str = pct.if_supports_color(Stdout, |p| p.style(dim));
    let tokens_str = tokens.if_supports_color(Stdout, |t| t.style(dim));
    let sep = "·".if_supports_color(Stdout, |s| s.style(dim));

    format!(
        "  {phase_str} → shift+tab {sep} {model_str} → ctrl+p {sep} {bar_str} {pct_str} {sep} {tokens_str} {sep} {slash}\n"
    )
}

/// Context bar: returns the *combined* string of filled + empty
/// cells, with a single trailing reset at the end of the empty
/// half so the colour stops cleanly. 20 cells total.
fn render_context_bar(pct: u8) -> String {
    let total_cells = 20usize;
    let pct_u = (pct as usize).min(100);
    let filled = (pct_u * total_cells) / 100;
    let empty = total_cells - filled;

    let dim = Style::new().dimmed();
    // Colour of the *filled* portion. We pick the colour based
    // on the percentage, not the filled-cell count, so the
    // threshold semantics are stable across terminals that
    // render block chars at different widths.
    let fill_style = if pct >= 85 {
        Style::new().red()
    } else if pct >= 60 {
        Style::new().yellow()
    } else {
        Style::new().green()
    };

    let filled_text = "█".repeat(filled);
    let empty_text = "░".repeat(empty);
    let filled_str = filled_text.if_supports_color(Stdout, |s| s.style(fill_style));
    let empty_str = empty_text.if_supports_color(Stdout, |s| s.style(dim));

    // The `if_supports_color` API emits a `\x1b[0m` at the end
    // of every coloured string. We want exactly one reset
    // between the filled and empty halves (and a final one
    // after the empty half). The simplest fix is to drop the
    // trailing reset of the filled half and let the empty
    // half's reset close out the whole bar.
    let filled_clean = strip_trailing_reset(filled_str.to_string());
    format!("{filled_clean}{empty_str}")
}

/// Strip a single trailing `\x1b[0m` from `s` (in-place style).
/// The `if_supports_color` helper unconditionally appends a
/// reset, but when we concatenate two coloured strings we only
/// want one reset in between them. This is the smallest possible
/// "ANSI combinator" — anything more general would mean pulling
/// in a full ANSI crate, which is overkill for one glyph.
fn strip_trailing_reset(mut s: String) -> String {
    if s.ends_with("\x1b[0m") {
        s.truncate(s.len() - 4);
    }
    s
}

/// Format `n` with thousands separators (`1,234,567`). The
/// status bar is the only place we display raw token counts,
/// and a 6-digit run of digits is hard to read at a glance.
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    // Walk from the right and insert a `,` every 3 digits.
    // We collect into a `Vec<char>` and reverse, which is
    // O(n) and avoids any string-allocation tricks.
    let chars: Vec<char> = s.chars().rev().collect();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> StatusState {
        StatusState {
            mode: Mode::Normal,
            model: "kimi-k2.6".into(),
            multi_model: false,
            context_pct: 25,
            input_tokens: 411_000,
            output_tokens: 6_900,
            slash_hint: "/ for commands".into(),
        }
    }

    #[test]
    fn mode_as_str_is_lowercase() {
        assert_eq!(Mode::Normal.as_str(), "normal");
        assert_eq!(Mode::Plan.as_str(), "plan");
        assert_eq!(Mode::Ferment.as_str(), "ferment");
    }

    #[test]
    fn render_status_bar_contains_all_required_segments() {
        let s = state();
        let out = render_status_bar(&s);
        let stripped = strip_ansi(&out);
        // Phase
        assert!(stripped.contains("normal"), "missing phase: {stripped}");
        // Model
        assert!(stripped.contains("kimi-k2.6"), "missing model: {stripped}");
        // Percentage
        assert!(stripped.contains("25% ctx"), "missing pct: {stripped}");
        // Token counters with thousands separators
        assert!(
            stripped.contains("↑411,000"),
            "missing input token counter: {stripped}"
        );
        assert!(
            stripped.contains("↓6,900"),
            "missing output token counter: {stripped}"
        );
        // Slash hint
        assert!(
            stripped.contains("/ for commands"),
            "missing slash hint: {stripped}"
        );
        // Key bindings
        assert!(stripped.contains("shift+tab"), "missing shift+tab hint");
        assert!(stripped.contains("ctrl+p"), "missing ctrl+p hint");
    }

    #[test]
    fn render_status_bar_emits_trailing_newline() {
        let s = state();
        let out = render_status_bar(&s);
        assert!(out.ends_with('\n'), "status bar must end in newline");
    }

    #[test]
    fn multi_model_tag_uses_multi_prefix() {
        let mut s = state();
        s.multi_model = true;
        let out = render_status_bar(&s);
        let stripped = strip_ansi(&out);
        assert!(
            stripped.contains("multi-model (kimi-k2.6)"),
            "multi-model tag missing: {stripped}"
        );
    }

    #[test]
    fn context_bar_full_when_pct_is_100() {
        let mut s = state();
        s.context_pct = 100;
        let out = render_status_bar(&s);
        let stripped = strip_ansi(&out);
        let filled = stripped.matches('█').count();
        let empty = stripped.matches('░').count();
        assert_eq!(filled, 20, "100% should fill all 20 cells, got {filled}");
        assert_eq!(empty, 0, "100% should have no empty cells, got {empty}");
    }

    #[test]
    fn context_bar_empty_when_pct_is_0() {
        let mut s = state();
        s.context_pct = 0;
        let out = render_status_bar(&s);
        let stripped = strip_ansi(&out);
        let filled = stripped.matches('█').count();
        let empty = stripped.matches('░').count();
        assert_eq!(filled, 0, "0% should have no filled cells, got {filled}");
        assert_eq!(empty, 20, "0% should have 20 empty cells, got {empty}");
    }

    #[test]
    fn context_bar_half_when_pct_is_50() {
        let mut s = state();
        s.context_pct = 50;
        let out = render_status_bar(&s);
        let stripped = strip_ansi(&out);
        let filled = stripped.matches('█').count();
        let empty = stripped.matches('░').count();
        // 50% of 20 is 10. The integer division in
        // `render_context_bar` rounds to 10 here exactly.
        assert_eq!(filled, 10, "50% should give 10 filled, got {filled}");
        assert_eq!(empty, 10, "50% should give 10 empty, got {empty}");
    }

    #[test]
    fn format_thousands_inserts_commas() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(1), "1");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(411_000), "411,000");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn strip_trailing_reset_removes_one_escape() {
        let s = "\x1b[32m██\x1b[0m".to_string();
        assert_eq!(strip_trailing_reset(s), "\x1b[32m██");
    }

    #[test]
    fn strip_trailing_reset_is_noop_when_no_reset() {
        let s = "no reset here".to_string();
        let s_clone = s.clone();
        let result = strip_trailing_reset(s);
        assert_eq!(result, s_clone);
    }

    /// Naive ANSI stripper used only by tests. Same logic as
    /// the one in `tui_header.rs` — duplicated here rather than
    /// hoisted to a shared `test_helpers` module because both
    /// files are standalone crates' worth of test code and the
    /// stripper is 15 lines.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut iter = s.chars().peekable();
        while let Some(c) = iter.next() {
            if c == '\x1b' {
                while let Some(&n) = iter.peek() {
                    iter.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
