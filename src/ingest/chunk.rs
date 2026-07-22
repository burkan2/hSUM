use crate::domain::{ByteSpan, LineSpan};
use std::path::Path;
use thiserror::Error;

pub const DEFAULT_CHUNK_TARGET_BYTES: usize = 1_200;
pub const DEFAULT_CHUNK_MAX_BYTES: usize = 1_800;
pub const DEFAULT_CHUNK_OVERLAP_BYTES: usize = 180;

const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChunkKind {
    Markdown,
    PlainText,
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
}

impl ChunkKind {
    pub const ALL: [Self; 7] = [
        Self::Markdown,
        Self::PlainText,
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::JavaScript,
        Self::Go,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::PlainText => "plain-text",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Go => "go",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        match extension {
            "md" | "markdown" => Some(Self::Markdown),
            "txt" => Some(Self::PlainText),
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" => Some(Self::JavaScript),
            "go" => Some(Self::Go),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChunkSettings {
    target_bytes: usize,
    max_bytes: usize,
    overlap_bytes: usize,
}

impl ChunkSettings {
    pub fn new(
        target_bytes: usize,
        max_bytes: usize,
        overlap_bytes: usize,
    ) -> Result<Self, ChunkSettingsError> {
        if target_bytes == 0 {
            return Err(ChunkSettingsError::ZeroTarget);
        }
        if max_bytes < target_bytes {
            return Err(ChunkSettingsError::MaximumBelowTarget);
        }
        if overlap_bytes >= target_bytes {
            return Err(ChunkSettingsError::OverlapDoesNotProgress);
        }
        Ok(Self {
            target_bytes,
            max_bytes,
            overlap_bytes,
        })
    }

    pub const fn target_bytes(self) -> usize {
        self.target_bytes
    }

    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    pub const fn overlap_bytes(self) -> usize {
        self.overlap_bytes
    }
}

impl Default for ChunkSettings {
    fn default() -> Self {
        Self {
            target_bytes: DEFAULT_CHUNK_TARGET_BYTES,
            max_bytes: DEFAULT_CHUNK_MAX_BYTES,
            overlap_bytes: DEFAULT_CHUNK_OVERLAP_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    ordinal: u32,
    span: ByteSpan,
    line_span: LineSpan,
    text: String,
}

impl Chunk {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    pub const fn line_span(&self) -> LineSpan {
        self.line_span
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

pub fn chunk_bytes(
    original: &[u8],
    kind: ChunkKind,
    settings: ChunkSettings,
) -> Result<Vec<Chunk>, ChunkError> {
    let text = std::str::from_utf8(original).map_err(|error| ChunkError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    if original.contains(&0) {
        return Err(ChunkError::NulContent);
    }

    let searchable_start = usize::from(original.starts_with(UTF8_BOM)) * UTF8_BOM.len();
    if searchable_start == original.len() {
        return Ok(Vec::new());
    }

    let newline_offsets: Vec<_> = original
        .iter()
        .enumerate()
        .filter_map(|(offset, byte)| (*byte == b'\n').then_some(offset))
        .collect();
    let mut chunks = Vec::new();
    let mut start = searchable_start;
    while start < original.len() {
        let hard_limit = start.saturating_add(settings.max_bytes).min(original.len());
        let hard_end = char_boundary_at_or_before(text, hard_limit, start);
        if hard_end <= start {
            return Err(ChunkError::BoundaryInvariant);
        }

        let end = if original.len() - start <= settings.target_bytes {
            hard_end
        } else {
            let target_limit = start.saturating_add(settings.target_bytes).min(hard_end);
            let target = char_boundary_at_or_before(text, target_limit, start);
            preferred_boundary(text, start, target, hard_end, kind).unwrap_or(hard_end)
        };
        if end <= start || end - start > settings.max_bytes {
            return Err(ChunkError::BoundaryInvariant);
        }

        let span =
            ByteSpan::new(start as u64, end as u64).map_err(|_| ChunkError::BoundaryInvariant)?;
        let line_span = line_span(&newline_offsets, start, end)?;
        chunks.push(Chunk {
            ordinal: u32::try_from(chunks.len()).map_err(|_| ChunkError::TooManyChunks)?,
            span,
            line_span,
            text: text[start..end].to_owned(),
        });

        if end == original.len() {
            break;
        }
        let proposed = end.saturating_sub(settings.overlap_bytes);
        let mut next = char_boundary_at_or_after(text, proposed, end);
        if next <= start {
            next = end;
        }
        start = next;
    }

    Ok(chunks)
}

fn preferred_boundary(
    text: &str,
    start: usize,
    target: usize,
    hard_end: usize,
    kind: ChunkKind,
) -> Option<usize> {
    let minimum = char_boundary_at_or_after(
        text,
        start.saturating_add((target.saturating_sub(start)) / 2),
        target,
    );
    let lines = line_records(text, start, hard_end);
    let paragraph_starts = paragraph_starts(&lines);
    let line_starts: Vec<_> = lines
        .iter()
        .map(|line| line.start)
        .filter(|position| *position > start)
        .collect();

    let priorities = match kind {
        ChunkKind::Markdown => vec![
            markdown_heading_starts(&lines),
            markdown_fence_starts(&lines),
            paragraph_starts,
            sentence_boundaries(text, start, hard_end),
        ],
        ChunkKind::PlainText => vec![
            paragraph_starts,
            sentence_boundaries(text, start, hard_end),
            line_starts,
        ],
        ChunkKind::Rust
        | ChunkKind::Python
        | ChunkKind::TypeScript
        | ChunkKind::JavaScript
        | ChunkKind::Go => vec![
            declaration_starts(&lines, kind),
            paragraph_starts,
            line_starts,
        ],
    };

    priorities
        .into_iter()
        .find_map(|candidates| choose_candidate(&candidates, minimum, target, hard_end))
}

fn choose_candidate(
    candidates: &[usize],
    minimum: usize,
    target: usize,
    hard_end: usize,
) -> Option<usize> {
    let mut before_or_at_target = None;
    let mut after_target = None;
    for &candidate in candidates {
        if candidate < minimum || candidate > hard_end {
            continue;
        }
        if candidate <= target {
            before_or_at_target = Some(candidate);
        } else if after_target.is_none() {
            after_target = Some(candidate);
        }
    }
    before_or_at_target.or(after_target)
}

#[derive(Clone, Copy)]
struct LineRecord<'a> {
    start: usize,
    content: &'a str,
}

fn line_records(text: &str, start: usize, end: usize) -> Vec<LineRecord<'_>> {
    let mut records = Vec::new();
    let mut line_start = text[..start].rfind('\n').map_or(0, |position| position + 1);
    while line_start < end {
        let line_end = text[line_start..end]
            .find('\n')
            .map_or(end, |offset| line_start + offset);
        let raw = &text[line_start..line_end];
        records.push(LineRecord {
            start: line_start,
            content: raw.strip_suffix('\r').unwrap_or(raw),
        });
        if line_end == end {
            break;
        }
        line_start = line_end + 1;
    }
    records
}

fn paragraph_starts(lines: &[LineRecord<'_>]) -> Vec<usize> {
    let mut candidates = Vec::new();
    let mut previous_blank = false;
    for line in lines {
        let blank = line.content.trim().is_empty();
        if previous_blank && !blank {
            candidates.push(line.start);
        }
        previous_blank = blank;
    }
    candidates
}

fn markdown_heading_starts(lines: &[LineRecord<'_>]) -> Vec<usize> {
    lines
        .iter()
        .filter(|line| line.content.trim_start().starts_with('#'))
        .map(|line| line.start)
        .collect()
}

fn markdown_fence_starts(lines: &[LineRecord<'_>]) -> Vec<usize> {
    lines
        .iter()
        .filter(|line| {
            let trimmed = line.content.trim_start();
            trimmed.starts_with("```") || trimmed.starts_with("~~~")
        })
        .map(|line| line.start)
        .collect()
}

fn declaration_starts(lines: &[LineRecord<'_>], kind: ChunkKind) -> Vec<usize> {
    lines
        .iter()
        .filter(|line| {
            if line.content.chars().next().is_some_and(char::is_whitespace) {
                return false;
            }
            let trimmed = line.content.trim_end();
            match kind {
                ChunkKind::Rust => starts_with_any(
                    trimmed,
                    &[
                        "fn ",
                        "pub fn ",
                        "pub(crate) fn ",
                        "struct ",
                        "pub struct ",
                        "enum ",
                        "pub enum ",
                        "trait ",
                        "pub trait ",
                        "impl ",
                        "mod ",
                        "pub mod ",
                    ],
                ),
                ChunkKind::Python => starts_with_any(trimmed, &["def ", "async def ", "class "]),
                ChunkKind::TypeScript => starts_with_any(
                    trimmed,
                    &[
                        "function ",
                        "export function ",
                        "export async function ",
                        "class ",
                        "export class ",
                        "interface ",
                        "export interface ",
                        "type ",
                        "export type ",
                    ],
                ),
                ChunkKind::JavaScript => starts_with_any(
                    trimmed,
                    &[
                        "function ",
                        "async function ",
                        "export function ",
                        "export async function ",
                        "class ",
                        "export class ",
                    ],
                ),
                ChunkKind::Go => starts_with_any(trimmed, &["func ", "type "]),
                ChunkKind::Markdown | ChunkKind::PlainText => false,
            }
        })
        .map(|line| line.start)
        .collect()
}

fn starts_with_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

fn sentence_boundaries(text: &str, start: usize, end: usize) -> Vec<usize> {
    let bytes = text.as_bytes();
    (start..end)
        .filter_map(|position| {
            let is_terminal = matches!(bytes[position], b'.' | b'?' | b'!');
            let boundary = position + 1;
            (is_terminal
                && (boundary == end
                    || bytes
                        .get(boundary)
                        .is_some_and(|byte| byte.is_ascii_whitespace())))
            .then_some(boundary)
        })
        .collect()
}

fn char_boundary_at_or_before(text: &str, mut position: usize, lower_bound: usize) -> usize {
    while position > lower_bound && !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

fn char_boundary_at_or_after(text: &str, mut position: usize, upper_bound: usize) -> usize {
    while position < upper_bound && !text.is_char_boundary(position) {
        position += 1;
    }
    position
}

fn line_span(newline_offsets: &[usize], start: usize, end: usize) -> Result<LineSpan, ChunkError> {
    let start_line = 1 + newline_offsets.partition_point(|offset| *offset < start);
    let final_byte = end.checked_sub(1).ok_or(ChunkError::BoundaryInvariant)?;
    let end_line = 1 + newline_offsets.partition_point(|offset| *offset < final_byte);
    LineSpan::new(start_line as u64, end_line as u64).map_err(|_| ChunkError::BoundaryInvariant)
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ChunkSettingsError {
    #[error("chunk target must be greater than zero")]
    ZeroTarget,
    #[error("chunk maximum must be at least the target")]
    MaximumBelowTarget,
    #[error("chunk overlap must be below the target so chunking always progresses")]
    OverlapDoesNotProgress,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ChunkError {
    #[error("content is not UTF-8 (valid through byte {valid_up_to})")]
    InvalidUtf8 { valid_up_to: usize },
    #[error("content contains a NUL byte")]
    NulContent,
    #[error("chunk boundary invariant failed")]
    BoundaryInvariant,
    #[error("document produces more chunks than the alpha.1 ordinal can represent")]
    TooManyChunks,
}
