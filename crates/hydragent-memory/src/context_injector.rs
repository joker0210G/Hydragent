use hydragent_types::MemoryDocument;
use tiktoken_rs::cl100k_base;

pub fn build_system_prompt_with_memory(
    base_prompt: &str,
    memories: &[MemoryDocument],
    max_memory_tokens: usize,
) -> String {
    if memories.is_empty() {
        return base_prompt.to_string();
    }

    let bpe = match cl100k_base() {
        Ok(b) => b,
        Err(_) => return base_prompt.to_string(),
    };

    let base_token_count = bpe.encode_with_special_tokens(base_prompt).len();

    let header = "\n\n---\n# Retrieved Long-term Memory\nThe following facts were retrieved from your persistent knowledge graph. They are ranked by relevance to the current conversation. Prioritize recent user input if it contradicts these facts.\n\n";
    let header_tokens = bpe.encode_with_special_tokens(header).len();

    let mut used_tokens = base_token_count + header_tokens;
    let mut memory_lines = Vec::new();
    let mut truncated = 0;

    for doc in memories {
        let ts = chrono::DateTime::from_timestamp_millis(doc.timestamp)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown date".to_string());

        let line = format!(
            "- [{}] (score={:.3}) {}\n",
            ts, doc.rrf_score, doc.content
        );

        let line_tokens = bpe.encode_with_special_tokens(&line).len();

        if (used_tokens - base_token_count) + line_tokens > max_memory_tokens {
            truncated += 1;
            continue;
        }

        memory_lines.push(line);
        used_tokens += line_tokens;
    }

    if truncated > 0 {
        tracing::debug!(
            truncated,
            max_memory_tokens,
            "Memory context truncated due to token budget"
        );
    }

    if memory_lines.is_empty() {
        return base_prompt.to_string();
    }

    format!("{}{}{}", base_prompt, header, memory_lines.join(""))
}
