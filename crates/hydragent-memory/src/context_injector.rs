use hydragent_types::MemoryDocument;
use tiktoken_rs::cl100k_base;

/// Build the system prompt with a single injected context block.
///
/// Per design spec §3 ("Single Injection"): Books, Shelves, and Pages are
/// compiled into **one ordered string** to prevent redundant context bloat.
///
/// The block is structured as:
///   Shelves (domain clusters) → Books (topic clusters) → Pages (session insights)
///
/// Graph-context docs (inserted by the Unified Hybrid Query Bridge) carry
/// prefixes `[Shelf / Domain]`, `[Book / Topic Cluster]`, and `[Page]` so
/// this injector can sort them into the right tier automatically.
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

    // Separate graph-tier docs from raw semantic memories
    let mut shelves: Vec<&MemoryDocument> = Vec::new();
    let mut books: Vec<&MemoryDocument>   = Vec::new();
    let mut pages: Vec<&MemoryDocument>   = Vec::new();
    let mut raw_mems: Vec<&MemoryDocument> = Vec::new();

    for doc in memories {
        if doc.content.starts_with("[Shelf / Domain]") {
            shelves.push(doc);
        } else if doc.content.starts_with("[Book / Topic Cluster]") {
            books.push(doc);
        } else if doc.content.starts_with("[Page]") {
            pages.push(doc);
        } else {
            raw_mems.push(doc);
        }
    }

    // Build ordered context sections (Shelf → Book → Page → raw facts)
    let base_token_count = bpe.encode_with_special_tokens(base_prompt).len();
    let header = "\n\n---\n# Library Knowledge Context\n\
        The following context was retrieved from your persistent Library knowledge graph \
        (Shelves → Books → Pages) and long-term memory. It is ranked by relevance. \
        Prioritize live conversation over these facts if they conflict.\n\n";
    let header_tokens = bpe.encode_with_special_tokens(header).len();
    let mut used_tokens = base_token_count + header_tokens;
    let mut lines: Vec<String> = Vec::new();

    // Helper: append a line if within budget
    let mut try_append = |line: String, used: &mut usize, lines: &mut Vec<String>| {
        let toks = bpe.encode_with_special_tokens(&line).len();
        if (*used - base_token_count) + toks <= max_memory_tokens {
            *used += toks;
            lines.push(line);
        }
    };

    // Shelves (dedup by id)
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for doc in &shelves {
        if seen.insert(doc.id.as_str()) {
            let line = format!("- {}\n", doc.content);
            try_append(line, &mut used_tokens, &mut lines);
        }
    }

    // Books
    for doc in &books {
        if seen.insert(doc.id.as_str()) {
            let line = format!("- {}\n", doc.content);
            try_append(line, &mut used_tokens, &mut lines);
        }
    }

    // Pages (session insights)
    for doc in &pages {
        if seen.insert(doc.id.as_str()) {
            let line = format!("- {}\n", doc.content);
            try_append(line, &mut used_tokens, &mut lines);
        }
    }

    // Raw semantic memory facts
    for doc in &raw_mems {
        if seen.insert(doc.id.as_str()) {
            let ts = chrono::DateTime::from_timestamp_millis(doc.timestamp)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown date".to_string());
            let line = format!(
                "- [{}] (score={:.3}) {}\n",
                ts, doc.rrf_score, doc.content
            );
            try_append(line, &mut used_tokens, &mut lines);
        }
    }

    if lines.is_empty() {
        return base_prompt.to_string();
    }

    format!("{}{}{}", base_prompt, header, lines.join(""))
}
