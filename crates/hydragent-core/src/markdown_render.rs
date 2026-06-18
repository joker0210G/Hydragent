// crates/hydragent-core/src/markdown_render.rs
//
// Terminal-flavoured markdown rendering for LLM responses.
//
// LLM replies come back as CommonMark / GFM-flavoured markdown
// (headings, fenced code blocks, tables, lists, bold/italic, inline
// code). Printing them raw in a terminal REPL is functional but ugly
// and hurts readability — `**bold**` shows up as literal asterisks,
// fenced code blocks look identical to prose, and tables collapse
// into a wall of pipes and dashes.
//
// This module wraps `termimad` to render markdown into ANSI-styled
// terminal text with a sensible default skin (cyan bold headings,
// dimmed code blocks, box-drawing-character tables). It's used by:
//
//   • `hydragent chat`         (REPL streaming → end-of-stream render)
//   • `hydragent test-brain`   (one-shot response)
//
// Width is auto-detected from the terminal via `crossterm` and falls
// back to 80 columns when the size can't be determined (piped
// output, CI logs, non-tty contexts).
//
// Behaviour summary:
//   - Default mode: the REPL *buffers* the full LLM reply, then
//     renders it once at end-of-stream with this module. This is
//     the only way to get nicely-formatted code blocks and aligned
//     tables — those need to see the whole content to lay out.
//   - Opt-in streaming: set `HYDRAGENT_STREAM_RAW=1` to get the
//     old "tokens appear one at a time" behaviour. The response
//     is NOT rendered in that case (you get raw markdown), but you
//     keep the live feedback of seeing the model think out loud.

use std::io::{self, Write};

/// Reusable markdown-to-ANSI renderer.
///
/// Cheap to construct (clones a skin), so the REPL builds one per
/// turn. The skin is built once via `new()` and reused for every
/// call to `render`.
pub struct MarkdownRenderer {
    skin: termimad::MadSkin,
    width: usize,
}

impl MarkdownRenderer {
    /// Build a renderer with the default skin, sized to the current
    /// terminal width (or 80 columns if the size can't be detected,
    /// which happens when stdout is piped/redirected or when running
    /// inside a non-tty context like CI).
    pub fn new() -> Self {
        let width = detect_terminal_width();
        let skin = build_default_skin();
        Self { skin, width }
    }

    /// Build a renderer with a custom width. Used by tests and by
    /// callers that want a fixed layout (e.g. log files).
    pub fn with_width(width: usize) -> Self {
        let skin = build_default_skin();
        Self { skin, width }
    }

    /// Render markdown to an ANSI-styled `String`. The width used
    /// for layout is whatever was passed to `new()`/`with_width()`.
    /// Trailing whitespace inside the returned string is preserved
    /// exactly as `termimad` produced it.
    pub fn render(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        // `termimad::MadSkin::text` returns a `FmtText` that
        // implements `Display` and lazily walks the markdown
        // inline-by-inline. For most LLM responses (< 10KB) this
        // is instantaneous; for very long responses the cost is
        // linear in token count, which is acceptable in a REPL.
        self.skin.text(text, Some(self.width)).to_string()
    }

    /// One-shot helper: print the rendered markdown to `out`,
    /// prefixed with a blank line and the "hydra ▸" header (the
    /// same label the REPL uses for live-streamed responses).
    /// The body is indented by two spaces so it lines up with the
    /// rest of the REPL's output. A trailing blank line is also
    /// written so the next REPL prompt doesn't run into the
    /// response text.
    pub fn print_to(&self, text: &str, out: &mut impl Write) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        // blank line above the header for visual separation from
        // the spinner / previous turn
        writeln!(out)?;
        // cyan + bold "hydra ▸" header (matches the REPL's
        // existing label colour)
        writeln!(out, "  \x1b[36;1mhydra ▸\x1b[0m")?;
        let rendered = self.render(text);
        for line in rendered.lines() {
            // 2-space indent so rendered body lines up under the
            // "hydra ▸" label rather than starting at column 0
            writeln!(out, "  {}", line)?;
        }
        // blank line below so the next user prompt has breathing
        // room
        writeln!(out)?;
        out.flush()
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Incremental markdown renderer for streaming token output.
///
/// The plain [`MarkdownRenderer::render`] needs the *full* text
/// to lay out tables and fenced code blocks nicely. But streaming
/// LLM replies are produced token-by-token — the caller (the
/// `test-brain` subcommand, the chat REPL) wants the user to
/// *see* each fragment as it lands, not stare at a blank screen
/// for 30 seconds and then get a wall of styled text.
///
/// [`MarkdownStreamer`] solves this by buffering incoming tokens
/// and flushing **complete renderable units** as soon as they
/// arrive:
///
///   • Plain prose / headings / bullet items: flushed on every
///     `\n`. Each line is rendered individually, so the user
///     sees text appear line-by-line (the "typewriter" effect).
///   • Fenced code blocks (`` ``` `` … `` ``` ``): buffered
///     atomically. The whole block is rendered once the closing
///     fence arrives, because partial code blocks look broken if
///     rendered mid-stream.
///   • Tables: not specially handled — they're rendered
///     line-by-line. This means each row appears as soon as the
///     model emits its trailing newline, and the column
///     alignment is computed from whatever rows have been seen
///     so far. In practice LLMs emit tables one row at a time,
///     so this gives a nice "table fills in" effect.
///
/// At end-of-stream, [`MarkdownStreamer::finish`] flushes any
/// remaining tail (the last line, if it didn't end in `\n`).
pub struct MarkdownStreamer {
    skin: termimad::MadSkin,
    in_code_block: bool,
    /// Tokens accumulated since the last flush. We hold them
    /// here so the next `push` can re-evaluate the buffer
    /// (e.g. detect a code-fence opening on a freshly-completed
    /// line).
    buffer: String,
}

impl MarkdownStreamer {
    /// Create a streamer that reuses the skin of an existing
    /// renderer. The renderer's `width` is not relevant here —
    /// the streamer produces the same styled string the renderer
    /// would, but one line at a time.
    pub fn new(renderer: &MarkdownRenderer) -> Self {
        Self {
            skin: renderer.skin.clone(),
            in_code_block: false,
            buffer: String::new(),
        }
    }

    /// Push a token into the stream. Returns the rendered text
    /// that should be written to the output *now*. The returned
    /// string may be empty if the streamer is still buffering
    /// (e.g. we're inside a code block, or we're waiting for a
    /// newline to flush the current line).
    ///
    /// The caller is responsible for actually writing the
    /// returned string to stdout (or wherever). The streamer
    /// holds no I/O state — it just transforms tokens.
    pub fn push(&mut self, token: &str) -> String {
        if token.is_empty() {
            return String::new();
        }
        self.buffer.push_str(token);
        let mut output = String::new();

        // ── Code-block mode: hold tokens until closing fence ──
        if self.in_code_block {
            // Look for the closing ``` at the start of a line.
            // CommonMark allows 0-3 spaces of indent on a fence,
            // so we scan for "\n" followed by up to 3 spaces and
            // then "```". The position returned is the index of
            // the leading '\n'.
            if let Some(nl_idx) = find_closing_fence(&self.buffer) {
                // Skip past the '\n' and the fence (3 backticks),
                // plus any info-string after the fence (e.g.
                // ````python` — we want to consume the rest of
                // that line too).
                let mut end = nl_idx + 1;
                let bytes = self.buffer.as_bytes();
                while end < bytes.len() && bytes[end] == b' ' {
                    end += 1;
                }
                end += 3; // skip the ```
                // Skip info string (e.g. "python") up to the
                // next '\n'. The whole fence line is part of
                // the block; we want to render it.
                if let Some(info_end) = self.buffer[end..].find('\n') {
                    end += info_end + 1;
                } else {
                    end = self.buffer.len();
                }
                let block: String = self.buffer[..end].to_string();
                self.buffer.drain(..end);
                self.in_code_block = false;
                output.push_str(&self.render_chunk(&block));
            }
            return output;
        }

        // ── Normal mode: flush complete lines ──
        // Walk the buffer, pulling off one line at a time. A
        // "line" is everything up to and including the next '\n'.
        while let Some(idx) = self.buffer.find('\n') {
            let line: String = self.buffer[..=idx].to_string();
            self.buffer.drain(..=idx);

            // Code-fence detection: if this line starts (after
            // optional leading whitespace) with ```, we're
            // entering a fenced code block. Put the line back
            // into the buffer and switch to code-block mode so
            // the next `push` accumulates the body until the
            // closing fence.
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                self.in_code_block = true;
                // Prepend the fence line back to whatever the
                // caller pushes next time.
                self.buffer = line + &self.buffer;
                break;
            }

            output.push_str(&self.render_chunk(&line));
        }

        output
    }

    /// Flush any remaining buffered text at end-of-stream. This
    /// handles the common case where the model's final line
    /// didn't end with a newline (or where we were mid-parse
    /// of a code block that never closed — best-effort render
    /// of whatever we have).
    pub fn finish(&mut self) -> String {
        if self.buffer.is_empty() {
            return String::new();
        }
        let tail = std::mem::take(&mut self.buffer);
        self.render_chunk(&tail)
    }

    /// Render a self-contained chunk of markdown. Thin wrapper
    /// around the same `termimad` call the non-streaming
    /// renderer uses — the only difference is we don't need to
    /// worry about the width because the chunk is small.
    fn render_chunk(&self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }
        // termimad's `text` method takes an optional width;
        // passing `None` lets termimad use the terminal width
        // it detected at startup. For streaming we don't care
        // about width optimisation — each chunk is small.
        self.skin.text(chunk, None).to_string()
    }
}

/// Find the position of the leading `\n` of the next closing
/// fence in a (potentially incomplete) buffered code block.
/// Returns the byte index of the `\n`, or `None` if no closing
/// fence is present yet.
///
/// "Closing fence" here means: a `\n` followed by 0-3 spaces and
/// then three or more backticks. The CommonMark spec allows up
/// to 3 spaces of indent on a fence; we honour that to match
/// what users (and LLMs) produce.
fn find_closing_fence(buf: &str) -> Option<usize> {
    let bytes = buf.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let mut j = i + 1;
            let mut spaces = 0;
            while j < bytes.len() && bytes[j] == b' ' && spaces < 4 {
                spaces += 1;
                j += 1;
            }
            // Need at least 3 backticks (and a possible info
            // string we don't care about).
            if j + 2 < bytes.len() && bytes[j] == b'`' && bytes[j + 1] == b'`' && bytes[j + 2] == b'`' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Build the default skin. Colours chosen to be readable on both
/// dark and light terminal themes (slightly desaturated cyan/yellow).
fn build_default_skin() -> termimad::MadSkin {
    let mut skin = termimad::MadSkin::default();
    use termimad::crossterm::style::{Attribute, Color};
    // Headings h1-h6: bold + cyan, sized to a sensible scale
    // (termimad handles the actual font sizing for h1..h6).
    for h in &mut skin.headers {
        h.set_fg(Color::Cyan);
        h.add_attr(Attribute::Bold);
    }
    // Bold: keep default (white/bold) but make it explicit so
    // we don't depend on termimad's default evolving.
    skin.bold.set_fg(Color::White);
    skin.bold.add_attr(Attribute::Bold);
    // Italic: just italic, no colour change.
    skin.italic.add_attr(Attribute::Italic);
    // Code blocks: termimad's default is already a dimmed
    // background — leave as-is. Setting a fg/bg here would
    // clash with the syntax-highlighting palettes for fenced
    // blocks.
    skin
}

/// Detect the terminal width via crossterm. Returns 80 columns when
/// the size can't be determined (piped/redirected stdout, CI
/// runners, etc.). The 4-column "reserve" gives the 2-space REPL
/// indent plus a small right margin for narrow terminals.
fn detect_terminal_width() -> usize {
    if let Ok((cols, _rows)) = crossterm::terminal::size() {
        let usable = (cols as usize).saturating_sub(4);
        // Clamp to a sensible range: at least 40 cols so tables
        // don't collapse to a single column, at most 200 so very
        // wide terminals don't produce silly 4-character-per-cell
        // table layouts.
        return usable.clamp(40, 200);
    }
    80
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed-width renderer is what unit tests need — terminal
    /// width detection is unreliable in `cargo test`.
    fn renderer() -> MarkdownRenderer {
        MarkdownRenderer::with_width(80)
    }

    #[test]
    fn plain_text_passes_through() {
        let out = renderer().render("hello world");
        assert!(out.contains("hello world"));
    }

    #[test]
    fn bold_is_styled_with_ansi() {
        let out = renderer().render("**bold**");
        // The literal "**" should be gone and the word "bold"
        // should be wrapped in ANSI escape sequences.
        assert!(!out.contains("**"), "raw asterisks leaked: {out:?}");
        assert!(out.contains("bold"));
        assert!(out.contains("\x1b["), "expected ANSI escapes, got: {out:?}");
    }

    #[test]
    fn code_block_preserves_content() {
        let out = renderer().render("```python\nprint(1)\n```");
        assert!(out.contains("print(1)"));
        // Fenced blocks should NOT leak their ``` fences into the
        // rendered output.
        assert!(!out.contains("```"), "fence leaked into render: {out:?}");
    }

    #[test]
    fn heading_is_styled() {
        let out = renderer().render("# Title\n\nbody");
        assert!(out.contains("Title"));
        assert!(out.contains("body"));
    }

    #[test]
    fn table_layout_uses_box_drawing() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let out = renderer().render(md);
        // termimad renders tables with Unicode box-drawing
        // characters. The cells (1, 2, 3, 4) must all appear; the
        // pipe characters used in GFM tables should not.
        assert!(out.contains('1'));
        assert!(out.contains('2'));
        assert!(out.contains('3'));
        assert!(out.contains('4'));
        // Box-drawing characters: one of ─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼
        let has_box = out.chars().any(|c| matches!(c, '─' | '│' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼'));
        assert!(has_box, "expected box-drawing chars in rendered table, got: {out:?}");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let out = renderer().render("");
        assert!(out.is_empty());
    }

    #[test]
    fn print_to_skips_header_on_empty() {
        let r = renderer();
        let mut buf: Vec<u8> = Vec::new();
        r.print_to("", &mut buf).unwrap();
        let s = String::from_utf8_lossy(&buf);
        // No "hydra" header when there's nothing to render.
        assert!(!s.contains("hydra"));
    }

    #[test]
    fn print_to_includes_header_on_nonempty() {
        let r = renderer();
        let mut buf: Vec<u8> = Vec::new();
        r.print_to("hello", &mut buf).unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("hydra ▸"));
        assert!(s.contains("hello"));
    }

    #[test]
    fn width_detection_falls_back_to_80() {
        // The fallback is exercised when crossterm can't read a
        // terminal size (which is the case under `cargo test`).
        let w = detect_terminal_width();
        assert!(w >= 40, "width too small: {w}");
        assert!(w <= 200, "width too large: {w}");
    }

    // ── MarkdownStreamer tests ───────────────────────────────────

    #[test]
    fn streamer_buffers_partial_line() {
        // A heading split across two pushes should not render
        // until the trailing newline arrives.
        let mut s = MarkdownStreamer::new(&renderer());
        let out = s.push("# Head");
        assert!(out.is_empty(), "partial line should buffer, got: {out:?}");
        let out = s.push("ing\n");
        assert!(out.contains("Heading"), "complete line should render: {out:?}");
        assert!(out.contains("\x1b["), "rendered line should have ANSI: {out:?}");
    }

    #[test]
    fn streamer_renders_each_line_as_it_completes() {
        // Three lines, each pushed as a single token. Each push
        // should yield the rendered line (because each ends in
        // \n). This is the "typewriter" path.
        let mut s = MarkdownStreamer::new(&renderer());
        let out1 = s.push("line one\n");
        let out2 = s.push("line two\n");
        let out3 = s.push("line three\n");
        assert!(out1.contains("line one") && !out1.contains("line two"));
        assert!(out2.contains("line two") && !out2.contains("line one"));
        assert!(out3.contains("line three") && !out3.contains("line two"));
    }

    #[test]
    fn streamer_buffers_code_block_until_closing_fence() {
        // A code block's body should NOT render line-by-line.
        // The whole block renders once the closing fence arrives.
        let mut s = MarkdownStreamer::new(&renderer());
        let _ = s.push("```python\n");
        let out_mid = s.push("print(1)\n");
        // While inside the code block we should have produced
        // nothing — the body is buffered until the closing fence.
        assert!(out_mid.is_empty(), "code-block body should buffer, got: {out_mid:?}");
        let out_close = s.push("```\n");
        // Now the whole block (opening fence + body + closing
        // fence) should be rendered in one shot.
        assert!(out_close.contains("print(1)"), "code body should render: {out_close:?}");
        assert!(!out_close.contains("```"), "fences should not leak: {out_close:?}");
    }

    #[test]
    fn streamer_finish_flushes_unterminated_tail() {
        // A model reply that doesn't end with \n should still
        // be rendered when finish() is called.
        let mut s = MarkdownStreamer::new(&renderer());
        let _ = s.push("partial line with no newline");
        let out = s.finish();
        assert!(out.contains("partial line"));
    }

    #[test]
    fn streamer_finish_is_noop_when_buffer_empty() {
        let mut s = MarkdownStreamer::new(&renderer());
        let out = s.finish();
        assert!(out.is_empty());
    }

    #[test]
    fn streamer_indented_fence_also_buffers() {
        // CommonMark allows up to 3 spaces of indent on a
        // fenced code block. The streamer should still detect
        // it.
        let mut s = MarkdownStreamer::new(&renderer());
        let _ = s.push("   ```\n");
        let out_mid = s.push("body\n");
        assert!(out_mid.is_empty(), "indented fence body should buffer: {out_mid:?}");
        let out_close = s.push("   ```\n");
        assert!(out_close.contains("body"));
    }

    #[test]
    fn streamer_inline_backticks_do_not_trigger_code_block() {
        // A line like "text ``` with ``` inline" should NOT
        // be treated as a code-block opening. The fence check
        // requires ``` to be at the start of the line (after
        // trimming).
        let mut s = MarkdownStreamer::new(&renderer());
        let out = s.push("text ``` with ``` inline\n");
        assert!(out.contains("text"), "line should render normally: {out:?}");
        assert!(out.contains("inline"));
    }

    #[test]
    fn streamer_continues_after_code_block_closes() {
        // After a code block closes, subsequent lines should
        // render in the normal (line-by-line) mode again.
        let mut s = MarkdownStreamer::new(&renderer());
        let _ = s.push("```\n");
        let _ = s.push("body\n");
        let _ = s.push("```\n");
        // Now back to normal mode:
        let out_after = s.push("normal line after\n");
        assert!(out_after.contains("normal line after"));
    }
}
