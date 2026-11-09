//! Deterministic project source chunking, lexical query construction, and
//! bounded HTML text extraction.
//!
//! This module never calls a model and never reads the filesystem. The engine
//! owns I/O so the chunk/query behavior stays testable and reproducible.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

pub const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_SOURCE_CHARS: usize = 2_000_000;
pub const TARGET_CHUNK_CHARS: usize = 1_400;
pub const MAX_CHUNK_CHARS: usize = 2_200;
pub const MIN_CHUNK_CHARS: usize = 180;
pub const CHUNK_OVERLAP_CHARS: usize = 160;
pub const MAX_CHUNKS_PER_SOURCE: usize = 500;
pub const MAX_QUERY_CHARS: usize = 400;
pub const MAX_QUERY_TERMS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDraft {
    pub content: String,
    pub heading: Option<String>,
    pub start_offset: usize,
    pub end_offset: usize,
    pub token_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceQuery {
    /// FTS5 expression that requires every term.
    pub all: String,
    /// FTS5 expression that accepts any term.
    pub any: String,
    pub terms: Vec<String>,
    pub raw: String,
    /// A CJK or very short query should also use the LIKE fallback because the
    /// unicode61 tokenizer can swallow an entire CJK run as one token.
    pub prefer_like: bool,
}

pub fn normalize_source_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split source text into overlapping, heading-aware chunks on safe character
/// boundaries. Blocks are kept together when they fit, and oversized blocks
/// are windowed rather than truncated.
pub fn chunk_text(text: &str) -> Vec<ChunkDraft> {
    let text = normalize_source_text(text);
    if text.trim().is_empty() || text.chars().count() > MAX_SOURCE_CHARS {
        return Vec::new();
    }
    let blocks = markdown_blocks(&text);
    let mut chunks = Vec::new();
    for block in blocks {
        if chunks.len() >= MAX_CHUNKS_PER_SOURCE {
            break;
        }
        if block.text.trim().is_empty() {
            continue;
        }
        if block.text.chars().count() <= MAX_CHUNK_CHARS {
            push_chunk(
                &mut chunks,
                &block.text,
                block.heading.clone(),
                block.start,
                block.end,
            );
        } else {
            split_large_block(&block, &mut chunks);
        }
    }
    chunks
}

#[derive(Debug)]
struct Block {
    text: String,
    heading: Option<String>,
    start: usize,
    end: usize,
}

fn markdown_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut block_start = 0usize;
    let mut block_text = String::new();
    let mut block_heading = heading_path(&headings);
    let mut in_fence = false;
    let mut fence = String::new();
    let mut offset = 0usize;

    let flush = |blocks: &mut Vec<Block>,
                 block_start: &mut usize,
                 block_text: &mut String,
                 block_heading: &Option<String>,
                 offset: usize,
                 next_start: usize| {
        if !block_text.trim().is_empty() {
            blocks.push(Block {
                text: std::mem::take(block_text),
                heading: block_heading.clone(),
                start: *block_start,
                end: offset,
            });
        } else {
            block_text.clear();
        }
        *block_start = next_start;
    };

    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let fence_marker = if in_fence {
            if trimmed.starts_with(&fence) {
                in_fence = false;
                fence.clear();
                true
            } else {
                false
            }
        } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = true;
            fence = if trimmed.starts_with("```") {
                "```".to_string()
            } else {
                "~~~".to_string()
            };
            true
        } else {
            false
        };

        if !in_fence
            && !fence_marker
            && let Some((level, title)) = markdown_heading(trimmed)
        {
            flush(
                &mut blocks,
                &mut block_start,
                &mut block_text,
                &block_heading,
                line_start,
                line_start,
            );
            headings.retain(|(existing, _)| *existing < level);
            headings.push((level, title));
            block_heading = heading_path(&headings);
        }

        block_text.push_str(line);
        if !in_fence && trim_blank(trimmed) && block_text.chars().count() >= MIN_CHUNK_CHARS {
            flush(
                &mut blocks,
                &mut block_start,
                &mut block_text,
                &block_heading,
                offset,
                offset,
            );
        }
    }
    flush(
        &mut blocks,
        &mut block_start,
        &mut block_text,
        &block_heading,
        offset,
        offset,
    );
    blocks
}

fn trim_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn markdown_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = trimmed[hashes..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, rest.trim().to_string()))
}

fn heading_path(headings: &[(usize, String)]) -> Option<String> {
    if headings.is_empty() {
        return None;
    }
    Some(
        headings
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>()
            .join(" / "),
    )
}

fn split_large_block(block: &Block, chunks: &mut Vec<ChunkDraft>) {
    let text = &block.text;
    let mut start = 0usize;
    while start < text.len() && chunks.len() < MAX_CHUNKS_PER_SOURCE {
        let remaining = text[start..].chars().count();
        if remaining <= MAX_CHUNK_CHARS {
            push_chunk(
                chunks,
                &text[start..],
                block.heading.clone(),
                block.start + start,
                block.end,
            );
            break;
        }
        let target = char_to_byte(text, start, MAX_CHUNK_CHARS);
        let search_start = char_to_byte(text, start, TARGET_CHUNK_CHARS);
        let hard_end = char_to_byte(text, start, MAX_CHUNK_CHARS);
        let end = find_split(text, search_start.max(start), target, hard_end).min(hard_end);
        if end <= start {
            break;
        }
        push_chunk(
            chunks,
            &text[start..end],
            block.heading.clone(),
            block.start + start,
            block.start + end,
        );
        let overlap = char_to_byte(text, start, CHUNK_OVERLAP_CHARS);
        let next = end.saturating_sub(overlap);
        start = if next <= start { end } else { next };
        // Prefer a clean line boundary after the overlap.
        if let Some(line_break) = text[start..end].rfind('\n') {
            let candidate = start + line_break + 1;
            if candidate < end {
                start = candidate;
            }
        }
    }
}

fn char_to_byte(text: &str, start: usize, chars: usize) -> usize {
    text[start..]
        .char_indices()
        .nth(chars)
        .map(|(offset, _)| start + offset)
        .unwrap_or(text.len())
}

fn find_split(text: &str, search_start: usize, target: usize, hard_end: usize) -> usize {
    let window = &text[search_start.min(hard_end)..hard_end.min(text.len())];
    if let Some(offset) = window.rfind("\n\n") {
        return search_start + offset + 2;
    }
    if let Some(offset) = window.rfind('\n') {
        return search_start + offset + 1;
    }
    if let Some(offset) = window.rfind(". ") {
        return search_start + offset + 2;
    }
    if let Some(offset) = window.rfind(' ') {
        return search_start + offset + 1;
    }
    let boundary = target.min(hard_end).min(text.len());
    let mut safe = boundary;
    while safe > search_start && !text.is_char_boundary(safe) {
        safe -= 1;
    }
    safe.max(search_start)
}

fn push_chunk(
    chunks: &mut Vec<ChunkDraft>,
    content: &str,
    heading: Option<String>,
    start: usize,
    end: usize,
) {
    let content = content.trim();
    if content.is_empty() {
        return;
    }
    let leading = content.as_ptr() as usize;
    let base = content;
    // `start`/`end` are offsets into the normalized source. Trimming can move
    // the content start; callers only need a useful range, so clamp and keep
    // the original block offsets when trimming is ambiguous.
    let _ = leading;
    let _ = base;
    chunks.push(ChunkDraft {
        content: content.to_string(),
        heading,
        start_offset: start,
        end_offset: end.max(start),
        token_count: estimate_tokens(content),
    });
}

pub fn estimate_tokens(text: &str) -> u32 {
    let mut words = 0u64;
    let mut cjk = 0u64;
    let mut in_word = false;
    for character in text.chars() {
        if is_cjk(character) {
            cjk += 1;
            in_word = false;
        } else if character.is_alphanumeric() || character == '_' {
            if !in_word {
                words += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    (words + cjk).min(u32::MAX as u64) as u32
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
    )
}

pub fn source_query(query: &str) -> Option<SourceQuery> {
    let raw = normalize_source_text(query).trim().to_string();
    if raw.is_empty() || raw.chars().count() > MAX_QUERY_CHARS {
        return None;
    }
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    let prefer_like = raw.chars().any(is_cjk);
    for term in tokenize(&raw) {
        if term.chars().count() < 2 && !term.chars().any(is_cjk) {
            continue;
        }
        if seen.insert(term.clone()) {
            terms.push(term);
            if terms.len() >= MAX_QUERY_TERMS {
                break;
            }
        }
    }
    if terms.is_empty() {
        return None;
    }
    let all = terms
        .iter()
        .map(|term| quote_fts(term))
        .collect::<Vec<_>>()
        .join(" AND ");
    let any = terms
        .iter()
        .map(|term| quote_fts(term))
        .collect::<Vec<_>>()
        .join(" OR ");
    Some(SourceQuery {
        all,
        any,
        terms,
        raw,
        prefer_like,
    })
}

fn quote_fts(term: &str) -> String {
    format!("\"{}\"*", term.replace('"', "\"\""))
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut cjk_run = String::new();
    let flush_word = |current: &mut String, tokens: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        let value = std::mem::take(current);
        for part in value.split(['_', '-', '.', '/', ':', '\\']) {
            if part.is_empty() {
                continue;
            }
            push_token(tokens, part);
            let mut camel = String::new();
            let mut previous_lower = false;
            for character in part.chars() {
                if character.is_uppercase() && previous_lower && !camel.is_empty() {
                    push_token(tokens, &camel);
                    camel.clear();
                }
                previous_lower = character.is_lowercase() || character.is_numeric();
                camel.push(character);
            }
            push_token(tokens, &camel);
        }
    };
    let flush_cjk = |cjk_run: &mut String, tokens: &mut Vec<String>| {
        let value = std::mem::take(cjk_run);
        if value.is_empty() {
            return;
        }
        push_token(tokens, &value);
        let chars = value.chars().collect::<Vec<_>>();
        for pair in chars.windows(2) {
            push_token(tokens, &format!("{}{}", pair[0], pair[1]));
        }
    };
    for character in input.chars() {
        if is_cjk(character) {
            flush_word(&mut current, &mut tokens);
            cjk_run.push(character);
        } else if character.is_alphanumeric()
            || character == '_'
            || character == '-'
            || character == '.'
            || character == '/'
            || character == ':'
            || character == '\\'
        {
            flush_cjk(&mut cjk_run, &mut tokens);
            current.push(character);
        } else {
            flush_word(&mut current, &mut tokens);
            flush_cjk(&mut cjk_run, &mut tokens);
        }
    }
    flush_word(&mut current, &mut tokens);
    flush_cjk(&mut cjk_run, &mut tokens);
    tokens
}

fn push_token(tokens: &mut Vec<String>, value: &str) {
    let value = value.trim().to_lowercase();
    if value.is_empty() || value.chars().count() > 80 {
        return;
    }
    tokens.push(value);
}

/// Convert a bounded HTML document into readable text. This is intentionally
/// conservative: scripts/styles/comments are removed, block tags become
/// newlines, and common entities are decoded. It is not a full DOM.
pub fn html_to_text(html: &str) -> String {
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    static BREAK: OnceLock<Regex> = OnceLock::new();
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let script = SCRIPT.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap());
    let style = STYLE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap());
    let comment = COMMENT.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    let br = BREAK.get_or_init(|| {
        Regex::new(r"(?i)</?(?:p|div|section|article|header|footer|li|tr|h[1-6]|br|pre|blockquote)\b[^>]*>")
            .unwrap()
    });
    let tags = TAGS.get_or_init(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    let text = script.replace_all(html, " ");
    let text = style.replace_all(&text, " ");
    let text = comment.replace_all(&text, " ");
    let text = br.replace_all(&text, "\n");
    let text = tags.replace_all(&text, " ");
    let text = decode_entities(&text);
    let mut output = String::with_capacity(text.len());
    let mut newlines = 0usize;
    for character in text.chars() {
        if character == '\n' {
            newlines += 1;
            if newlines <= 2 {
                output.push('\n');
            }
        } else if character.is_whitespace() {
            if !output.ends_with(' ') && !output.ends_with('\n') {
                output.push(' ');
            }
            newlines = 0;
        } else {
            output.push(character);
            newlines = 0;
        }
    }
    output.trim().to_string()
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            output.push_str(rest);
            return output;
        };
        if end > 12 {
            output.push('&');
            rest = &rest[1..];
            continue;
        }
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix('#')
                .and_then(|value| {
                    if let Some(hex) = value.strip_prefix('x').or_else(|| value.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        value.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            output.push(character);
            rest = &rest[end + 1..];
        } else {
            output.push('&');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_markdown_with_headings_and_offsets() {
        let text = "# Alpha\n\nFirst paragraph with enough words to survive the tiny block merge rule. It should stay with the heading.\n\n## Beta\n\nSecond paragraph with a different idea and enough words to be useful.\n";
        let chunks = chunk_text(text);
        assert!(!chunks.is_empty());
        assert!(
            chunks[0]
                .heading
                .as_deref()
                .unwrap_or_default()
                .contains("Alpha")
        );
        assert!(chunks.iter().any(|chunk| {
            chunk
                .heading
                .as_deref()
                .unwrap_or_default()
                .contains("Alpha / Beta")
        }));
        for chunk in &chunks {
            assert!(chunk.start_offset <= chunk.end_offset);
            assert!(!chunk.content.trim().is_empty());
        }
    }

    #[test]
    fn splits_oversized_blocks_with_overlap_and_char_boundaries() {
        let text = "word ".repeat(1_200);
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.content.chars().count() <= MAX_CHUNK_CHARS)
        );
        assert!(chunks.iter().all(|chunk| chunk.token_count > 0));
    }

    #[test]
    fn query_terms_cover_cjk_and_code_tokens() {
        let query = source_query("Fix LoginController.test 登录 失败").unwrap();
        assert!(query.prefer_like);
        assert!(query.terms.iter().any(|term| term == "logincontroller"));
        assert!(query.terms.iter().any(|term| term == "登录"));
        assert!(query.all.contains(" AND "));
        assert!(query.any.contains(" OR "));
    }

    #[test]
    fn html_extraction_removes_active_content_and_decodes_entities() {
        let html = "<html><head><style>x</style><script>alert(1)</script></head><body><h1>Title</h1><p>A &amp; B&#39;s test.</p><!-- secret --></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("A & B's test."));
        assert!(!text.contains("alert"));
        assert!(!text.contains("secret"));
    }
}
