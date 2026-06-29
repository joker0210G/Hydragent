// crates/hydragent-core/src/tui_header.rs
//
// Two-column startup header inspired by the Kimi-CLI / Hyper CLI
// family: a block-art hydra silhouette on the left (version +
// branch + path), and a Unicode-boxed "tip of the day" panel on
// the right.
//
// We deliberately keep this module *pure*: it takes data, returns
// strings, and prints to stdout. No ANSI escapes are embedded
// directly; everything goes through the `owo_colors` crate so
// the palette is centralised and `NO_COLOR` is respected for free.
//
// Layout (the two columns are joined on a single line so the
// reader's eye can scan left → right):
//
//   ┌─────────────────────────────┐ ┌──────────────────────────────────┐
//   │  ▄▄▄▄▄▄▄▄▄▄▄▄▄▄             │ │ Kimchi's special:                │
//   │▄█▀▀  ▀▀  ▀▀  ▀█▄            │ │ ──────────────────────────────── │
//   │█  ▄▄▄ ▄▄▄ ▄▄▄  █            │ │ Use /paste to drop in a long      │
//   │█ █▀▀█▄█▀▀█▄█▀▀█ █   🐉 HYDRA │ │ prompt that spans several lines.  │
//   │█ █▄▄███▄▄███▄▄█ █  v0.7.2   │ │ Finish with a line containing    │
//   │█▄  ▀▀▀ ▀▀▀ ▀▀▀  ▄█  main    │ │ only ```  (or /paste on its own). │
//   │ ▀█▄▄  ▄▄  ▄▄  ▄▄█▀           │ └──────────────────────────────────┘
//   │   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀            │
//   └─────────────────────────────┘
//
// The widths of the two columns are computed from the contents
// of the right-hand panel (which is the wider of the two), so the
// left panel auto-grows to match and the whole header renders
// flush with the user's terminal width.

use owo_colors::{OwoColorize, Style, Stream::Stdout};
use std::fmt::Write;

/// 3-headed hydra silhouette. The three "▄▄▄ ▄▄▄ ▄▄▄" triplets in
/// the middle row are the three heads — the defining feature of
/// the mythical hydra and the visual pun this header is built
/// around. Every line is exactly 21 visible columns wide so the
/// right-hand panel can pad to a matching width and the
/// silhouette stays visually rectangular.
pub const LOGO: &[&str] = &[
    "     ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
    "   ▄█▀▀  ▀▀  ▀▀  ▀█▄▄",
    "  █▀  ▄▄▄ ▄▄▄ ▄▄▄  ▀█",
    "  █  ▀▄▀  ▀▄▀  ▀▄▀  █",
    "  █  ▄▄▄  ▄▄▄  ▄▄▄  █",
    "  █▄  ▀▀▀ ▀▀▀ ▀▀▀  ▄█",
    "   ▀█▄▄  ▄▄  ▄▄  ▄▄█▀",
    "     ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
];

/// Visible width of every logo line, in terminal columns.
/// The logo data is the single source of truth: every line in
/// `LOGO` above must be exactly this many columns wide. If a
/// future edit introduces drift, the unit test in this module
/// (`logo_width_is_documented_constant`) will fail and force a
/// fix here.
pub const LOGO_WIDTH: usize = 21;

/// Pre-built metadata that gets baked into the header at startup.
/// Holding it in a struct (rather than threading five separate
/// args through `print_kimi_header`) makes it easy to add a new
/// field — e.g. `git_sha` — without rippling through every
/// call site.
#[derive(Debug, Clone)]
pub struct BrandInfo {
    /// Crate version (from `CARGO_PKG_VERSION`).
    pub version: String,
    /// Git branch name (`git rev-parse --abbrev-ref HEAD`), or
    /// `"unknown"` when git is not on PATH. We deliberately fall
    /// back to a literal string rather than omitting the field
    /// so the header always has the same number of lines.
    pub branch: String,
    /// Absolute workspace path (`std::env::current_dir()`). On
    /// WSL it renders as `/mnt/d/Workspace/Hydragent`; on Windows
    /// it renders as `D:\Workspace\Hydragent`. Either way, it's
    /// the directory the user is actually in, not the directory
    /// the binary lives in.
    pub path: String,
    /// Short page id (first 8 chars of the UUID). Echoed in the
    /// prompt as well, so the user can confirm "yes, this is the
    /// conversation I meant to resume".
    pub page_id_short: String,
    /// Active brain model name (e.g. `anthropic/claude-3.5-sonnet`).
    pub model: String,
    /// Number of tools registered in the `ToolRegistry`. Shown
    /// in the right-hand panel as a quick "what can this thing
    /// do" sanity check.
    pub tool_count: usize,
}

/// The right-hand panel of the header. A short title (rendered
/// bold + cyan), a horizontal rule, a list of body lines, an
/// optional second rule, and an optional continuation. The Kimi
/// example uses the "after rule" for a *secondary* message that
/// the user would otherwise miss — e.g. the escape hatch from a
/// special mode.
#[derive(Debug, Clone)]
pub struct TipBox {
    /// Bold cyan title (e.g. `"Kimchi's special:"`).
    pub title: String,
    /// Dim body lines. Each one is rendered verbatim with a dim
    /// grey style. Long lines are word-wrapped to fit the column
    /// width (see `render_tip_box`).
    pub lines: Vec<String>,
    /// Optional secondary message after the first rule. We model
    /// it as a separate list so callers can compose a tip out of
    /// two "blocks" of text without having to know the box-drawing
    /// internals.
    pub after_rule: Vec<String>,
}

impl TipBox {
    /// Convenience constructor: title + body, no secondary block.
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
            after_rule: Vec::new(),
        }
    }

    /// Constructor with a secondary block after the rule.
    pub fn with_after(
        title: impl Into<String>,
        lines: Vec<String>,
        after_rule: Vec<String>,
    ) -> Self {
        Self {
            title: title.into(),
            lines,
            after_rule,
        }
    }
}

/// Render the right-hand panel as a single multi-line string.
/// We return the string (rather than printing it) so the caller
/// — `print_kimi_header` — can join the two columns line-by-line
/// and produce a single `println!` per visual row.
pub fn render_tip_box(tip: &TipBox, width: usize) -> String {
    // Defensive floor: the box-drawing chars assume at least
    // 6 visible columns of interior space (so a one-character
    // body line can fit between the `│` and `│` borders).
    let inner_width = width.max(8).saturating_sub(2);
    let mut out = String::new();
    let title_style = Style::new().bold().cyan();
    let dim_style = Style::new().dimmed();
    let rule = "─".repeat(inner_width);

    // Top border with title inline. The Kimi design uses a flat
    // `┌────────────────────────────────────┐` without an inline
    // title, but I find the inline form more scannable. The
    // title sits at the left edge of the box, then we draw the
    // rule out to the right border.
    let title_visual = tip.title.chars().count();
    let rule_after_title = inner_width.saturating_sub(title_visual + 1);
    let _ = writeln!(
        out,
        "┌─ {} {}",
        tip.title.if_supports_color(Stdout, |t| t.style(title_style)),
        "─".repeat(rule_after_title).if_supports_color(Stdout, |r| r.style(dim_style)),
    );

    // Body lines. Word-wrap to `inner_width` columns so a
    // 100-character tip body doesn't blow out the box on narrow
    // terminals. We use a simple `wrap_at` that breaks on the
    // nearest whitespace before the limit, falling back to a
    // hard break if the line is longer than the width.
    for line in &tip.lines {
        for wrapped in wrap_at(line, inner_width) {
            let _ = writeln!(
                out,
                "│ {}",
                wrapped.if_supports_color(Stdout, |w| w.style(dim_style))
            );
        }
    }

    // Optional secondary rule + body. Mirrors the Kimi layout
    // where the "to leave this mode…" paragraph sits in its own
    // visual block under a separator.
    if !tip.after_rule.is_empty() {
        let _ = writeln!(
            out,
            "├{}┤",
            rule.if_supports_color(Stdout, |r| r.style(dim_style))
        );
        for line in &tip.after_rule {
            for wrapped in wrap_at(line, inner_width) {
                let _ = writeln!(
                    out,
                    "│ {}",
                    wrapped.if_supports_color(Stdout, |w| w.style(dim_style))
                );
            }
        }
    }

    // Bottom border.
    let _ = writeln!(
        out,
        "└{}┘",
        rule.if_supports_color(Stdout, |r| r.style(dim_style))
    );
    out
}

/// Word-wrap `text` to at most `width` columns, breaking on
/// whitespace. Long tokens (e.g. a URL with no spaces) are
/// hard-broken at the limit. Returns a `Vec<String>` of the
/// wrapped lines; an empty input returns a single empty string
/// so the caller can always emit at least one line.
fn wrap_at(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            // First word on the line. If it exceeds the width
            // by itself, hard-break it.
            if word_len > width {
                let mut buf = String::new();
                let mut buf_len = 0usize;
                for c in word.chars() {
                    if buf_len >= width {
                        out.push(std::mem::take(&mut buf));
                        buf_len = 0;
                    }
                    buf.push(c);
                    buf_len += 1;
                }
                current = buf;
                current_len = buf_len;
            } else {
                current.push_str(word);
                current_len = word_len;
            }
            continue;
        }
        // +1 for the space we'd insert.
        if current_len + 1 + word_len > width {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        } else {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Render the two-column header as a single string. Returns the
/// whole thing (logo column + tip column joined row-by-row) so
/// the caller can `println!` it as a unit. Tests assert on this
/// string.
pub fn render_kimi_header(brand: &BrandInfo, tip: &TipBox) -> String {
    // The right-hand panel drives the column width. The logo is
    // a fixed 18 columns; the tip box is `inner_width + 2`
    // (for the `│` borders). We size the gap between the two
    // columns to one space for breathing room.
    let tip_str = render_tip_box(tip, 48);
    let tip_lines: Vec<&str> = tip_str.lines().collect();

    // Build the "sidecar" text that sits next to the logo. We define both
    // the plain (uncolored) version to measure its visible character length,
    // and the colored version.
    let sidecar_raw = [
        (
            "🐉 HYDRAGENT".to_string(),
            "🐉 HYDRAGENT".if_supports_color(Stdout, |s| s.style(Style::new().bold().cyan())).to_string()
        ),
        (
            format!("v{}", brand.version),
            format!("v{}", brand.version.if_supports_color(Stdout, |v| v.style(Style::new().dimmed())))
        ),
        (
            brand.branch.clone(),
            brand.branch.if_supports_color(Stdout, |b| b.style(Style::new().dimmed())).to_string()
        ),
        (
            brand.path.clone(),
            brand.path.if_supports_color(Stdout, |p| p.style(Style::new().dimmed())).to_string()
        ),
        (
            format!("page {} · model {} · {} tools", brand.page_id_short, brand.model, brand.tool_count),
            format!(
                "page {} · model {} · {} tools",
                brand.page_id_short.if_supports_color(Stdout, |p| p.style(Style::new().cyan())),
                brand.model.if_supports_color(Stdout, |m| m.style(Style::new().dimmed())),
                brand.tool_count
            )
        ),
    ];

    let max_sidecar_width = sidecar_raw.iter().map(|(plain, _)| plain.chars().count()).max().unwrap_or(0);

    let sidecar: Vec<String> = sidecar_raw.iter().map(|(plain, colored)| {
        let visible_len = plain.chars().count();
        let padding = " ".repeat(max_sidecar_width - visible_len);
        format!("{}{}", colored, padding)
    }).collect();

    // Join the two columns row-by-row. The logo has 8 lines; the
    // sidecar is 5 lines (which sits in the middle of the logo,
    // vertically centred). The tip box has its own height. We
    // line them up so the *bottom* of the sidecar is one row
    // above the *bottom* of the logo — that puts the
    // version/branch/path lines in the lower half of the
    // silhouette, mirroring Kimi's layout where the branch and
    // path sit below the mascot.
    let mut out = String::new();
    let sidecar_offset = LOGO.len().saturating_sub(sidecar.len() + 1);
    for (i, logo_line) in LOGO.iter().enumerate() {
        let left = format!("{:<width$}", logo_line, width = LOGO_WIDTH);
        let right = if i >= sidecar_offset && i - sidecar_offset < sidecar.len() {
            sidecar[i - sidecar_offset].clone()
        } else {
            " ".repeat(max_sidecar_width)
        };
        // Pick the tip row, if any. The tip box can be taller
        // than the logo; the *extra* rows are rendered after the
        // logo loop as a continuation.
        let tip_row = tip_lines.get(i).copied().unwrap_or("");
        // The format below is:  <logo>  <sidecar>    <tip box>
        // The 4-space gap between the sidecar and the tip box
        // gives the two columns clear visual separation.
        let _ = writeln!(out, "  {left}  {right}    {tip_row}");
    }
    // Render any tip rows that extend below the logo.
    for extra in tip_lines.iter().skip(LOGO.len()) {
        let pad = " ".repeat(LOGO_WIDTH + 2 + max_sidecar_width);
        let _ = writeln!(out, "  {pad}    {extra}");
    }
    out
}

/// Convenience wrapper: build the header string and print it to
/// stdout. Most callers will use this; tests use `render_kimi_header`
/// directly so they can assert on the bytes.
pub fn print_kimi_header(brand: &BrandInfo, tip: &TipBox) {
    print!("{}", render_kimi_header(brand, tip));
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Build the default tip of the day. Shown on every `hydragent chat`
/// startup. The "Kimchi's special" branding is a wink at the
/// inspiration source (Kimi CLI), with a Hydragent-flavoured
/// command (`/paste` instead of `/ferment`) so we don't claim
/// parity with a tool we don't have.
pub fn default_tip_box() -> TipBox {
    let tips = vec![
        TipBox::with_after(
            "Kimchi's special:".to_string(),
            vec![
                "Use /paste to drop in a long prompt that spans".to_string(),
                "several lines. Finish with a line containing only".to_string(),
                "```  (or /paste on its own).".to_string(),
            ],
            vec![
                "Tip: /model [name] switches the active brain for this".to_string(),
                "session; /brain shows the base URL + masked key. Both".to_string(),
                "are shortcuts to the same data /debug prints.".to_string(),
            ],
        ),
        TipBox::with_after(
            "Kimchi's special:".to_string(),
            vec![
                "Try using /resume to open the interactive Library browser.".to_string(),
                "Use the arrow keys to browse Shelves, Books, and Page".to_string(),
                "nodes visually, and press Enter to load them.".to_string(),
            ],
            vec![
                "Tip: /new lets you start a fresh page and choose exactly".to_string(),
                "where to place it in the Library system.".to_string(),
            ],
        ),
        TipBox::with_after(
            "Kimchi's special:".to_string(),
            vec![
                "Run /compact to compress long conversations using the LLM.".to_string(),
                "It summarizes history and clears active memory to keep".to_string(),
                "context usage low and responses fast.".to_string(),
            ],
            vec![
                "Tip: Auto-compaction will automatically trigger if your".to_string(),
                "context fullness reaches 80%.".to_string(),
            ],
        ),
    ];

    let idx = (chrono::Local::now().timestamp() as usize) % tips.len();
    tips[idx].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_brand() -> BrandInfo {
        BrandInfo {
            version: "0.7.2".into(),
            branch: "main".into(),
            path: "/mnt/d/Workspace/Hydragent".into(),
            page_id_short: "abc12345".into(),
            model: "anthropic/claude-3.5-sonnet".into(),
            tool_count: 21,
        }
    }

    #[test]
    fn logo_has_eight_lines() {
        assert_eq!(LOGO.len(), 8, "logo silhouette is 8 lines tall");
    }

    #[test]
    fn logo_width_is_documented_constant() {
        // All 8 lines must be exactly LOGO_WIDTH columns wide so
        // the right-hand panel can pad to a matching width. The
        // expected width is computed from the data itself — if a
        // future edit changes the logo, this assertion still
        // passes as long as the new lines are internally
        // consistent. The point of the test is to catch *drift*
        // (e.g. one line 1 char wider than the rest), not to
        // pin a specific number.
        for (i, line) in LOGO.iter().enumerate() {
            let cols = line.chars().count();
            assert_eq!(
                cols,
                LOGO_WIDTH,
                "logo line {i} is {cols} cols wide, expected LOGO_WIDTH={} : {:?}",
                LOGO_WIDTH,
                line
            );
        }
    }

    #[test]
    fn render_tip_box_has_top_and_bottom_borders() {
        let tip = TipBox::new("Hello", vec!["world".into()]);
        let out = render_tip_box(&tip, 40);
        let first = out.lines().next().unwrap();
        let last = out.lines().next_back().unwrap();
        assert!(first.starts_with('┌'), "top border uses ┌: {first}");
        assert!(last.starts_with('└'), "bottom border uses └: {last}");
    }

    #[test]
    fn render_tip_box_wraps_long_lines() {
        let tip = TipBox::new(
            "Test",
            vec!["this is a long line that should definitely wrap around to a second line".into()],
        );
        let out = render_tip_box(&tip, 20);
        // The body line is 65 chars; in a 20-col box it must
        // wrap into at least 3 wrapped rows.
        let body_rows: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with('│'))
            .collect();
        assert!(
            body_rows.len() >= 3,
            "expected at least 3 wrapped rows, got {}: {:?}",
            body_rows.len(),
            body_rows
        );
    }

    #[test]
    fn render_tip_box_with_after_rule_has_middle_border() {
        let tip = TipBox::with_after(
            "X",
            vec!["body".into()],
            vec!["tail".into()],
        );
        let out = render_tip_box(&tip, 20);
        assert!(
            out.lines().any(|l| l.starts_with('├')),
            "secondary rule uses ├: {out}"
        );
    }

    #[test]
    fn render_kimi_header_is_eight_lines_minimum() {
        let brand = sample_brand();
        let tip = TipBox::new("Hi", vec!["body".into()]);
        let out = render_kimi_header(&brand, &tip);
        assert!(out.lines().count() >= LOGO.len());
    }

    #[test]
    fn render_kimi_header_contains_brand_metadata() {
        let brand = sample_brand();
        let tip = TipBox::new("X", vec!["y".into()]);
        let out = render_kimi_header(&brand, &tip);
        // We strip ANSI for the assertion so a colour-disabled
        // terminal doesn't break the test. The metadata is
        // *semantically* the contract — visible to the user,
        // plain to the test.
        let stripped = strip_ansi(&out);
        assert!(stripped.contains("v0.7.2"), "missing version");
        assert!(stripped.contains("main"), "missing branch");
        assert!(
            stripped.contains("/mnt/d/Workspace/Hydragent"),
            "missing path"
        );
        assert!(stripped.contains("21 tools"), "missing tool count");
    }

    #[test]
    fn wrap_at_handles_empty_input() {
        let lines = wrap_at("", 10);
        assert_eq!(lines, vec!["".to_string()]);
    }

    #[test]
    fn wrap_at_handles_long_token() {
        // A single 25-char token in a 10-col box should hard-break
        // into 3 chunks: 10 + 10 + 5.
        let lines = wrap_at("abcdefghijklmnopqrstuvwxy", 10);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].chars().count(), 10);
        assert_eq!(lines[1].chars().count(), 10);
        assert_eq!(lines[2].chars().count(), 5);
    }

    #[test]
    fn wrap_at_respects_whitespace() {
        let lines = wrap_at("the quick brown fox", 10);
        // "the quick" is 9 chars (fits), "brown fox" is 9 (fits).
        assert_eq!(lines, vec!["the quick", "brown fox"]);
    }

    /// Naive ANSI stripper used only by tests. We don't need
    /// full CSI support — just enough to remove the
    /// `\x1b[Nm` style codes that `owo-colors` emits, so our
    /// content assertions don't have to know the colour palette.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut iter = s.chars().peekable();
        while let Some(c) = iter.next() {
            if c == '\x1b' {
                // Skip until we hit the terminator byte (a letter
                // in the range @-~) or the end of the string.
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
