use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Estratégia de chunking para divisão inteligente de texto/conteúdo.
pub trait Chunker: Send + Sync {
    fn chunk(&self, content: &str) -> Vec<Chunk>;
}

/// Chunk result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub content: String,
    pub start_pos: usize,
    pub end_pos: usize,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Fixed-size chunking (by character count).
pub struct FixedSizeChunker {
    size: usize,
    overlap: usize,
}

impl FixedSizeChunker {
    pub fn new(size: usize, overlap: usize) -> Self {
        assert!(size > 0, "size must be greater than 0");
        assert!(overlap < size, "overlap must be less than size");
        Self { size, overlap }
    }
}

impl Chunker for FixedSizeChunker {
    fn chunk(&self, content: &str) -> Vec<Chunk> {
        let chars: Vec<(usize, char)> = content.char_indices().collect();
        let total = chars.len();
        let step = self.size - self.overlap;
        let mut chunks = Vec::new();
        let mut i = 0;
        while i < total {
            let end = (i + self.size).min(total);
            let start_byte = chars[i].0;
            let end_byte = if end == total {
                content.len()
            } else {
                chars[end].0
            };
            chunks.push(Chunk {
                id: format!("chunk-{}", chunks.len()),
                content: content[start_byte..end_byte].to_string(),
                start_pos: start_byte,
                end_pos: end_byte,
                metadata: HashMap::new(),
            });
            if end == total {
                break;
            }
            i += step;
        }
        chunks
    }
}

/// Recursive chunking (divide por delimitadores hierárquicos).
pub struct RecursiveChunker {
    separators: Vec<String>,
    chunk_size: usize,
}

impl RecursiveChunker {
    pub fn new(chunk_size: usize) -> Self {
        Self {
            separators: vec!["\n\n".into(), "\n".into(), " ".into(), "".into()],
            chunk_size,
        }
    }

    fn chunk_internal(&self, text: &str, separators: &[String], offset: usize) -> Vec<Chunk> {
        if separators.is_empty() {
            if text.chars().count() <= self.chunk_size {
                return vec![Chunk {
                    id: format!("chunk-{}", offset),
                    content: text.to_string(),
                    start_pos: offset,
                    end_pos: offset + text.len(),
                    metadata: HashMap::new(),
                }];
            }
            return FixedSizeChunker::new(self.chunk_size, 0)
                .chunk(text)
                .into_iter()
                .map(|mut c| {
                    c.start_pos += offset;
                    c.end_pos += offset;
                    c
                })
                .collect();
        }
        let sep = &separators[0];
        let rest = &separators[1..];
        if sep.is_empty() {
            return self.chunk_internal(text, rest, offset);
        }

        let mut chunks = Vec::new();
        let mut last_end = 0usize;
        let mut had_match = false;
        for (match_start, matched) in text.match_indices(sep) {
            had_match = true;
            let piece = &text[last_end..match_start + matched.len()];
            if !piece.is_empty() {
                let piece_offset = offset + last_end;
                let sub = self.chunk_internal(piece, rest, piece_offset);
                chunks.extend(sub);
            }
            last_end = match_start + matched.len();
        }
        if had_match && last_end < text.len() {
            let piece = &text[last_end..];
            let piece_offset = offset + last_end;
            let sub = self.chunk_internal(piece, rest, piece_offset);
            chunks.extend(sub);
        }
        if !had_match {
            return self.chunk_internal(text, rest, offset);
        }
        chunks
    }
}

impl Chunker for RecursiveChunker {
    fn chunk(&self, content: &str) -> Vec<Chunk> {
        self.chunk_internal(content, &self.separators, 0)
    }
}

/// Semantic chunking (chunk por sentenças/parágrafos semânticos).
pub struct SemanticChunker {
    min_size: usize,
    max_size: usize,
}

impl SemanticChunker {
    pub fn new(min_size: usize, max_size: usize) -> Self {
        assert!(max_size > 0, "max_size must be greater than 0");
        Self { min_size, max_size }
    }
}

fn sentence_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (i, c) in text.char_indices() {
        if c == '.' || c == '!' || c == '?' {
            let mut end = i + c.len_utf8();
            while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            ranges.push((start, end));
            start = end;
        }
    }
    if start < text.len() {
        ranges.push((start, text.len()));
    }
    ranges
}

impl Chunker for SemanticChunker {
    fn chunk(&self, content: &str) -> Vec<Chunk> {
        let ranges = sentence_ranges(content);
        let mut chunks = Vec::new();
        let mut pending_start = 0usize;
        let mut pending_end = 0usize;
        let mut pending_len = 0usize;

        for (s, e) in ranges {
            let sent = &content[s..e];
            let sent_len = sent.chars().count();

            if sent_len > self.max_size {
                if pending_len > 0 {
                    chunks.push(Chunk {
                        id: format!("chunk-{}", chunks.len()),
                        content: content[pending_start..pending_end].to_string(),
                        start_pos: pending_start,
                        end_pos: pending_end,
                        metadata: HashMap::new(),
                    });
                    pending_len = 0;
                }
                let sub = FixedSizeChunker::new(self.max_size, 0).chunk(sent);
                for mut c in sub {
                    c.start_pos += s;
                    c.end_pos += s;
                    c.id = format!("chunk-{}", chunks.len());
                    chunks.push(c);
                }
                pending_start = e;
                pending_end = e;
                continue;
            }

            if pending_len == 0 {
                pending_start = s;
                pending_end = e;
                pending_len = sent_len;
            } else if pending_len + sent_len > self.max_size {
                chunks.push(Chunk {
                    id: format!("chunk-{}", chunks.len()),
                    content: content[pending_start..pending_end].to_string(),
                    start_pos: pending_start,
                    end_pos: pending_end,
                    metadata: HashMap::new(),
                });
                pending_start = s;
                pending_end = e;
                pending_len = sent_len;
            } else {
                // merge only if one side is below min_size
                if sent_len < self.min_size || pending_len < self.min_size {
                    pending_end = e;
                    pending_len += sent_len;
                } else {
                    chunks.push(Chunk {
                        id: format!("chunk-{}", chunks.len()),
                        content: content[pending_start..pending_end].to_string(),
                        start_pos: pending_start,
                        end_pos: pending_end,
                        metadata: HashMap::new(),
                    });
                    pending_start = s;
                    pending_end = e;
                    pending_len = sent_len;
                }
            }
        }

        if pending_len > 0 {
            chunks.push(Chunk {
                id: format!("chunk-{}", chunks.len()),
                content: content[pending_start..pending_end].to_string(),
                start_pos: pending_start,
                end_pos: pending_end,
                metadata: HashMap::new(),
            });
        }

        chunks
    }
}

/// Markdown-aware chunking (preserva estrutura de headers).
pub struct MarkdownChunker {
    max_size: usize,
}

impl MarkdownChunker {
    pub fn new(max_size: usize) -> Self {
        assert!(max_size > 0, "max_size must be greater than 0");
        Self { max_size }
    }
}

impl Chunker for MarkdownChunker {
    fn chunk(&self, content: &str) -> Vec<Chunk> {
        let line_slices: Vec<&str> = content.split_inclusive('\n').collect();
        let mut chunks = Vec::new();
        let mut i = 0;
        let mut offset = 0usize;

        while i < line_slices.len() {
            let line = line_slices[i];
            if line.trim_start().starts_with('#') {
                let level = line.trim_start().chars().take_while(|c| *c == '#').count();
                let start = offset;
                let mut j = i + 1;
                while j < line_slices.len() {
                    let next_line = line_slices[j];
                    if next_line.trim_start().starts_with('#') {
                        let next_level = next_line
                            .trim_start()
                            .chars()
                            .take_while(|c| *c == '#')
                            .count();
                        if next_level <= level {
                            break;
                        }
                    }
                    j += 1;
                }
                let section = line_slices[i..j].concat();
                let end = start + section.len();
                if section.chars().count() <= self.max_size {
                    chunks.push(Chunk {
                        id: format!("chunk-{}", chunks.len()),
                        content: section,
                        start_pos: start,
                        end_pos: end,
                        metadata: HashMap::new(),
                    });
                } else {
                    let sub = RecursiveChunker::new(self.max_size).chunk(&section);
                    for mut c in sub {
                        c.start_pos += start;
                        c.end_pos += start;
                        c.id = format!("chunk-{}", chunks.len());
                        chunks.push(c);
                    }
                }
                offset = end;
                i = j;
            } else {
                let start = offset;
                let mut j = i;
                while j < line_slices.len() && !line_slices[j].trim_start().starts_with('#') {
                    j += 1;
                }
                let section = line_slices[i..j].concat();
                let end = start + section.len();
                if section.chars().count() <= self.max_size {
                    chunks.push(Chunk {
                        id: format!("chunk-{}", chunks.len()),
                        content: section,
                        start_pos: start,
                        end_pos: end,
                        metadata: HashMap::new(),
                    });
                } else {
                    let sub = RecursiveChunker::new(self.max_size).chunk(&section);
                    for mut c in sub {
                        c.start_pos += start;
                        c.end_pos += start;
                        c.id = format!("chunk-{}", chunks.len());
                        chunks.push(c);
                    }
                }
                offset = end;
                i = j;
            }
        }
        chunks
    }
}

/// Code-aware chunking (preserva funções/classes/blocks).
pub struct CodeChunker {
    max_size: usize,
}

impl CodeChunker {
    pub fn new(max_size: usize) -> Self {
        assert!(max_size > 0, "max_size must be greater than 0");
        Self { max_size }
    }
}

impl Chunker for CodeChunker {
    fn chunk(&self, content: &str) -> Vec<Chunk> {
        let re = regex::Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+").unwrap();
        let matches: Vec<_> = re.find_iter(content).collect();
        let mut chunks = Vec::new();
        let mut last_end = 0usize;

        for (idx, m) in matches.iter().enumerate() {
            let start = m.start();
            let end = matches
                .get(idx + 1)
                .map(|mm| mm.start())
                .unwrap_or(content.len());

            if start > last_end {
                let preamble = &content[last_end..start];
                if !preamble.trim().is_empty() {
                    let preamble_len = preamble.chars().count();
                    if preamble_len <= self.max_size {
                        chunks.push(Chunk {
                            id: format!("chunk-{}", chunks.len()),
                            content: preamble.to_string(),
                            start_pos: last_end,
                            end_pos: start,
                            metadata: HashMap::new(),
                        });
                    } else {
                        let sub = RecursiveChunker::new(self.max_size).chunk(preamble);
                        for mut c in sub {
                            c.start_pos += last_end;
                            c.end_pos += last_end;
                            c.id = format!("chunk-{}", chunks.len());
                            chunks.push(c);
                        }
                    }
                }
            }

            let block = &content[start..end];
            let block_len = block.chars().count();
            if block_len <= self.max_size {
                chunks.push(Chunk {
                    id: format!("chunk-{}", chunks.len()),
                    content: block.to_string(),
                    start_pos: start,
                    end_pos: end,
                    metadata: HashMap::new(),
                });
            } else {
                let sub = RecursiveChunker::new(self.max_size).chunk(block);
                for mut c in sub {
                    c.start_pos += start;
                    c.end_pos += start;
                    c.id = format!("chunk-{}", chunks.len());
                    chunks.push(c);
                }
            }
            last_end = end;
        }

        if last_end < content.len() {
            let trailing = &content[last_end..];
            if !trailing.trim().is_empty() {
                let trailing_len = trailing.chars().count();
                if trailing_len <= self.max_size {
                    chunks.push(Chunk {
                        id: format!("chunk-{}", chunks.len()),
                        content: trailing.to_string(),
                        start_pos: last_end,
                        end_pos: content.len(),
                        metadata: HashMap::new(),
                    });
                } else {
                    let sub = RecursiveChunker::new(self.max_size).chunk(trailing);
                    for mut c in sub {
                        c.start_pos += last_end;
                        c.end_pos += last_end;
                        c.id = format!("chunk-{}", chunks.len());
                        chunks.push(c);
                    }
                }
            }
        }

        chunks
    }
}

// ── ChunkStore ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct ChunkStoreData {
    chunks: HashMap<String, Chunk>,
    index: HashMap<String, Vec<String>>,
}

/// Armazena chunks com índice para busca rápida.
pub struct ChunkStore {
    blackboard: Blackboard,
    chunks: HashMap<String, Chunk>,
    index: HashMap<String, Vec<String>>,
}

impl ChunkStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            chunks: HashMap::new(),
            index: HashMap::new(),
        }
    }

    /// Adiciona chunks ao store.
    pub fn store(&mut self, source_id: &str, chunks: Vec<Chunk>) -> Result<()> {
        for (i, mut chunk) in chunks.into_iter().enumerate() {
            chunk.id = format!("{}::chunk-{}", source_id, i);
            chunk.metadata.insert(
                "source_id".to_string(),
                serde_json::Value::String(source_id.to_string()),
            );
            self.index_chunk(&chunk);
            self.chunks.insert(chunk.id.clone(), chunk);
        }
        Ok(())
    }

    /// Busca chunks por termo (simples FTS).
    pub fn search(&self, query: &str) -> Vec<&Chunk> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for term in &terms {
            if let Some(ids) = self.index.get(term) {
                for id in ids {
                    *counts.entry(id.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted
            .into_iter()
            .filter_map(|(id, _)| self.chunks.get(&id))
            .collect()
    }

    /// Recupera chunk por ID.
    pub fn get(&self, chunk_id: &str) -> Option<&Chunk> {
        self.chunks.get(chunk_id)
    }

    /// Remove chunks de uma fonte.
    pub fn remove_source(&mut self, source_id: &str) -> Result<()> {
        let ids_to_remove: Vec<String> = self
            .chunks
            .values()
            .filter(|c| c.metadata.get("source_id").and_then(|v| v.as_str()) == Some(source_id))
            .map(|c| c.id.clone())
            .collect();
        for id in &ids_to_remove {
            self.chunks.remove(id);
        }
        self.rebuild_index();
        Ok(())
    }

    /// Persiste no Blackboard.
    pub fn persist(&self) -> Result<()> {
        let data = ChunkStoreData {
            chunks: self.chunks.clone(),
            index: self.index.clone(),
        };
        self.blackboard
            .put_tuple("chunkstore", "data", serde_json::to_value(data)?)
    }

    /// Carrega do Blackboard.
    pub fn load(&mut self) -> Result<()> {
        if let Some(value) = self.blackboard.get_tuple("chunkstore", "data") {
            let data: ChunkStoreData = serde_json::from_value(value).unwrap_or_default();
            self.chunks = data.chunks;
            self.index = data.index;
        }
        Ok(())
    }

    fn index_chunk(&mut self, chunk: &Chunk) {
        for term in chunk.content.to_lowercase().split_whitespace() {
            self.index
                .entry(term.to_string())
                .or_default()
                .push(chunk.id.clone());
        }
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        let values: Vec<Chunk> = self.chunks.values().cloned().collect();
        for chunk in &values {
            self.index_chunk(chunk);
        }
    }
}

// ── ChunkPipeline ─────────────────────────────────────────────────────────────

/// Pipeline completo: detectar tipo → escolher chunker → chunk → store.
pub struct ChunkPipeline {
    chunk_store: ChunkStore,
}

impl ChunkPipeline {
    pub fn new(chunk_store: ChunkStore) -> Self {
        Self { chunk_store }
    }

    /// Detecta o tipo de conteúdo e escolhe o chunker apropriado.
    pub fn auto_chunk(&mut self, source_id: &str, content: &str) -> Result<Vec<Chunk>> {
        let chunker: Box<dyn Chunker> = if Self::is_markdown(content) {
            Box::new(MarkdownChunker::new(2000))
        } else if Self::is_code(content) {
            Box::new(CodeChunker::new(2000))
        } else {
            Box::new(SemanticChunker::new(200, 2000))
        };
        self.chunk_with(source_id, content, chunker)
    }

    /// Chunk com estratégia específica.
    pub fn chunk_with(
        &mut self,
        source_id: &str,
        content: &str,
        chunker: Box<dyn Chunker>,
    ) -> Result<Vec<Chunk>> {
        let chunks = chunker.chunk(content);
        self.chunk_store.store(source_id, chunks.clone())?;
        Ok(chunks)
    }

    fn is_markdown(content: &str) -> bool {
        content.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ")
        }) || content.contains("```")
    }

    fn is_code(content: &str) -> bool {
        content.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("fn ")
                || trimmed.starts_with("pub fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("def ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
        })
    }
}

// ── Reasoning Chunker (compilação de raciocínio em regras) ───────────────────

use crate::meta_cognitive::ReasoningStep;
use crate::procedural::ProductionRule;

/// Compila uma sequência de passos de raciocínio em uma regra de produção.
pub struct ReasoningChunker;

impl ReasoningChunker {
    /// Extrai padrão, diagnóstico e correção de uma cadeia de raciocínio.
    /// - pattern: input do primeiro passo.
    /// - diagnosis: output do passo intermediário (ou do primeiro se < 3 passos).
    /// - correction: output do último passo.
    pub fn chunk(reasoning_steps: &[ReasoningStep]) -> Result<ProductionRule> {
        if reasoning_steps.is_empty() {
            return Err(anyhow::anyhow!("lista de passos vazia"));
        }
        let pattern = if reasoning_steps[0].input.is_empty() {
            reasoning_steps[0].output.clone()
        } else {
            reasoning_steps[0].input.clone()
        };
        let diagnosis = if reasoning_steps.len() >= 3 {
            reasoning_steps[reasoning_steps.len() / 2].output.clone()
        } else {
            reasoning_steps[0].output.clone()
        };
        let correction = reasoning_steps.last().unwrap().output.clone();
        Ok(ProductionRule {
            id: format!("chunk-{}", reasoning_steps[0].id),
            pattern,
            diagnosis,
            correction,
            success_rate: 0.5,
        })
    }
}

/// Lei da Potência da Prática (Power Law of Practice) — ativação baseada em
/// recência e frequência.
pub struct PowerLawOfPractice;

impl PowerLawOfPractice {
    /// Decay fixo de 0.001 por segundo (~3.6 por hora).
    const DECAY: f64 = 0.001;

    /// Calcula a ativação de uma regra.
    /// Fórmula: A = ln(use_count) - decay * (now - last_used)
    pub fn activation(_rule_id: &str, last_used: u64, use_count: u64) -> f64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if use_count == 0 {
            f64::NEG_INFINITY
        } else {
            let recency = now.saturating_sub(last_used) as f64;
            (use_count as f64).ln() - Self::DECAY * recency
        }
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn fixed_size_chunker_basic() {
        let chunker = FixedSizeChunker::new(10, 0);
        let text = "Hello world, this is a test of fixed size chunking.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.content.chars().count() <= 10);
        }
    }

    #[test]
    fn fixed_size_chunker_with_overlap() {
        let chunker = FixedSizeChunker::new(10, 2);
        let text = "Hello world, this is a test of overlap.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() > 1);
        for i in 0..chunks.len().saturating_sub(1) {
            let a = &chunks[i];
            let b = &chunks[i + 1];
            assert!(b.start_pos < a.end_pos, "chunks must overlap");
        }
    }

    #[test]
    fn recursive_chunker_hierarchical() {
        let chunker = RecursiveChunker::new(50);
        let text = "Paragraph one.\n\nParagraph two.\n\nParagraph three.";
        let chunks = chunker.chunk(text);
        let combined: String = chunks.iter().map(|c| c.content.clone()).collect();
        assert_eq!(combined, text);
        assert!(chunks.len() >= 3);
    }

    #[test]
    fn semantic_chunker_sentence_boundary() {
        let chunker = SemanticChunker::new(10, 100);
        let text = "First sentence. Second sentence. Third sentence is here.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            let trimmed = c.content.trim();
            assert!(
                trimmed.ends_with('.')
                    || trimmed.ends_with('!')
                    || trimmed.ends_with('?')
                    || c.end_pos == text.len()
            );
        }
    }

    #[test]
    fn markdown_chunker_preserves_headers() {
        let chunker = MarkdownChunker::new(2000);
        let text = "# Header 1\nContent 1\n## Subheader\nSub content\n# Header 2\nContent 2";
        let chunks = chunker.chunk(text);
        let combined: String = chunks.iter().map(|c| c.content.clone()).collect();
        assert_eq!(combined, text);
        assert!(chunks.iter().any(|c| c.content.starts_with("# Header 1")));
        assert!(chunks.iter().any(|c| c.content.starts_with("# Header 2")));
    }

    #[test]
    fn code_chunker_preserves_functions() {
        let chunker = CodeChunker::new(2000);
        let text =
            "fn foo() {\n    println!(\"foo\");\n}\n\nfn bar() {\n    println!(\"bar\");\n}\n";
        let chunks = chunker.chunk(text);
        assert!(chunks.iter().any(|c| c.content.contains("fn foo")));
        assert!(chunks.iter().any(|c| c.content.contains("fn bar")));
    }

    #[test]
    fn chunk_store_add_and_search() {
        let bb = temp_bb();
        let mut store = ChunkStore::new(bb);
        let chunks = vec![
            Chunk {
                id: "c1".into(),
                content: "hello world".into(),
                start_pos: 0,
                end_pos: 11,
                metadata: HashMap::new(),
            },
            Chunk {
                id: "c2".into(),
                content: "rust code".into(),
                start_pos: 12,
                end_pos: 21,
                metadata: HashMap::new(),
            },
        ];
        store.store("src", chunks).unwrap();
        let results = store.search("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "hello world");
    }

    #[test]
    fn chunk_store_remove_source() {
        let bb = temp_bb();
        let mut store = ChunkStore::new(bb);
        store
            .store(
                "src",
                vec![Chunk {
                    id: "c1".into(),
                    content: "hello".into(),
                    start_pos: 0,
                    end_pos: 5,
                    metadata: HashMap::new(),
                }],
            )
            .unwrap();
        store
            .store(
                "doc",
                vec![Chunk {
                    id: "c2".into(),
                    content: "world".into(),
                    start_pos: 0,
                    end_pos: 5,
                    metadata: HashMap::new(),
                }],
            )
            .unwrap();
        store.remove_source("src").unwrap();
        assert!(store.search("hello").is_empty());
        assert_eq!(store.search("world").len(), 1);
    }

    #[test]
    fn chunk_pipeline_auto_detects_markdown() {
        let bb = temp_bb();
        let mut pipeline = ChunkPipeline::new(ChunkStore::new(bb));
        let md = "# Title\n\nSome paragraph.\n\n## Section\nMore text.";
        let chunks = pipeline.auto_chunk("doc1", md).unwrap();
        assert!(chunks.iter().any(|c| c.content.contains("# Title")));
        assert!(chunks.iter().any(|c| c.content.contains("## Section")));
    }

    #[test]
    fn chunk_pipeline_auto_detects_code() {
        let bb = temp_bb();
        let mut pipeline = ChunkPipeline::new(ChunkStore::new(bb));
        let code = "fn main() {\n    println!(\"hello\");\n}\n";
        let chunks = pipeline.auto_chunk("src1", code).unwrap();
        assert!(chunks.iter().any(|c| c.content.contains("fn main")));
    }

    #[test]
    fn chunk_store_persist_and_load() {
        let f = NamedTempFile::new().unwrap();
        let p = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        let mut store = ChunkStore::new(bb);
        store
            .store(
                "src",
                vec![Chunk {
                    id: "c1".into(),
                    content: "persist test".into(),
                    start_pos: 0,
                    end_pos: 12,
                    metadata: HashMap::new(),
                }],
            )
            .unwrap();
        store.persist().unwrap();

        let bb2 = Blackboard::open(&p).unwrap();
        let mut store2 = ChunkStore::new(bb2);
        store2.load().unwrap();
        let results = store2.search("persist");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "persist test");
    }

    #[test]
    fn chunk_overlap_consistency() {
        let chunker = FixedSizeChunker::new(20, 5);
        let text = "abcdefghijklmnopqrstuvwxyz0123456789";
        let chunks = chunker.chunk(text);
        for i in 0..chunks.len().saturating_sub(1) {
            let a = &chunks[i];
            let b = &chunks[i + 1];
            let overlap_text = &text[b.start_pos..a.end_pos];
            assert_eq!(overlap_text.chars().count(), 5);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Testes ReasoningChunker
    // ═══════════════════════════════════════════════════════════════════════════════

    fn make_reasoning_step(id: &str, phase: &str, input: &str, output: &str) -> ReasoningStep {
        ReasoningStep {
            id: id.into(),
            phase: phase.into(),
            input: input.into(),
            output: output.into(),
            confidence: 0.8,
            timestamp: 1,
        }
    }

    #[test]
    fn chunker_compila_reasoning_steps_em_regra() {
        let steps = vec![
            make_reasoning_step(
                "1",
                "observe",
                "erro de borrow",
                "lifetime inválido detectado",
            ),
            make_reasoning_step(
                "2",
                "orient",
                "lifetime inválido detectado",
                "adicionar anotação 'a",
            ),
        ];
        let rule = ReasoningChunker::chunk(&steps).unwrap();
        assert_eq!(rule.pattern, "erro de borrow");
        assert_eq!(rule.diagnosis, "lifetime inválido detectado");
        assert_eq!(rule.correction, "adicionar anotação 'a");
    }

    #[test]
    fn chunker_extrai_padrao_correto_de_tres_passos() {
        let steps = vec![
            make_reasoning_step(
                "1",
                "observe",
                "panic em runtime",
                "índice fora dos limites",
            ),
            make_reasoning_step(
                "2",
                "diagnose",
                "índice fora dos limites",
                "acesso inseguro ao vetor",
            ),
            make_reasoning_step(
                "3",
                "fix",
                "acesso inseguro ao vetor",
                "usar get() com bounds check",
            ),
        ];
        let rule = ReasoningChunker::chunk(&steps).unwrap();
        assert_eq!(rule.pattern, "panic em runtime");
        assert_eq!(rule.diagnosis, "acesso inseguro ao vetor");
        assert_eq!(rule.correction, "usar get() com bounds check");
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Testes PowerLawOfPractice
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn powerlaw_uso_frequente_ativacao_alta() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a_frequente = PowerLawOfPractice::activation("r1", now, 100);
        let a_raro = PowerLawOfPractice::activation("r2", now, 2);
        assert!(
            a_frequente > a_raro,
            "uso frequente deve ter ativação maior"
        );
    }

    #[test]
    fn powerlaw_uso_antigo_ativacao_baixa() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a_recente = PowerLawOfPractice::activation("r1", now, 10);
        let a_antigo = PowerLawOfPractice::activation("r2", now.saturating_sub(10_000), 10);
        assert!(a_recente > a_antigo, "uso recente deve ter ativação maior");
    }
}
