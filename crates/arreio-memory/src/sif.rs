use crate::envelope::MemoryEnvelope;
use crate::recall::RecallResult;

/// Frame de contexto montado pelo SIF Assembler.
#[derive(Debug, Clone)]
pub struct SifContextFrame {
    pub text: String,
    pub tokens_used: usize,
    pub memories_included: Vec<String>,
}

/// Assembler de contexto compacto (SIF — Structured Inference Frame).
/// Monta um frame de texto respeitando um budget de tokens.
pub struct SifAssembler {
    max_tokens: usize,
    tokens_per_char: f32,
}

impl SifAssembler {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            tokens_per_char: 0.25, // heurística: ~4 chars por token para inglês/português
        }
    }

    /// Monta o contexto a partir dos resultados de recall.
    /// Inclui memórias por ordem de score até esgotar o budget.
    pub fn assemble(
        &self,
        results: &[RecallResult],
        memories: &[MemoryEnvelope],
    ) -> SifContextFrame {
        self.assemble_with_project(results, memories, None)
    }

    /// Monta o contexto a partir dos resultados de recall, incluindo memória durável de projeto.
    pub fn assemble_with_project(
        &self,
        results: &[RecallResult],
        memories: &[MemoryEnvelope],
        project_content: Option<&str>,
    ) -> SifContextFrame {
        let mut text = String::new();
        let mut tokens_used: usize = 0;
        let mut included = Vec::new();

        text.push_str("## Contexto de Memória Relevante\n\n");

        for result in results {
            // Project memory é tratado especialmente (não é MemoryEnvelope)
            if result.memory_id == "project-memory" {
                if let Some(pc) = project_content {
                    let entry = format!(
                        "[ProjectMemory] score={:.2} why={:?}\n{}\n\n",
                        result.score, result.why_retrieved, pc
                    );
                    let entry_tokens = (entry.len() as f32 * self.tokens_per_char) as usize;
                    if tokens_used + entry_tokens <= self.max_tokens {
                        text.push_str(&entry);
                        tokens_used += entry_tokens;
                        included.push("project-memory".into());
                    }
                }
                continue;
            }

            if let Some(mem) = memories.iter().find(|m| m.id == result.memory_id) {
                let entry = format!(
                    "[M{}] tipo={:?} score={:.2} why={:?}\n{}\n\n",
                    mem.id,
                    mem.memory_type,
                    result.score,
                    result.why_retrieved,
                    mem.primary_text().unwrap_or("(sem texto)")
                );
                let entry_tokens = (entry.len() as f32 * self.tokens_per_char) as usize;

                if tokens_used + entry_tokens > self.max_tokens {
                    break;
                }

                text.push_str(&entry);
                tokens_used += entry_tokens;
                included.push(mem.id.clone());
            }
        }

        if included.is_empty() {
            text.clear();
        }

        SifContextFrame {
            text,
            tokens_used,
            memories_included: included,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{MemoryEnvelope, MemoryType, ModalityRef, Scope};

    fn make_mem(id: &str, text: &str) -> MemoryEnvelope {
        MemoryEnvelope {
            id: id.into(),
            scope: Scope::default(),
            memory_type: MemoryType::Semantic,
            modalities: vec![ModalityRef {
                modality_type: "text".into(),
                content: text.into(),
            }],
            importance: 0.8,
            confidence: 0.9,
            entities: vec![],
            tags: vec![],
            content_hash: "abc".into(),
            created_at: 0,
        }
    }

    #[test]
    fn assemble_respeita_budget() {
        let assembler = SifAssembler::new(50);
        let mems = vec![
            make_mem("m1", "fato importante sobre autenticação"),
            make_mem("m2", "outro fato muito extenso que deveria ocupar muitos tokens se fosse incluído completamente sem truncamento adequado"),
        ];
        let results = vec![
            RecallResult {
                memory_id: "m1".into(),
                score: 1.0,
                confidence: 0.9,
                why_retrieved: vec!["fts".into()],
                layer_signals: Default::default(),
            },
            RecallResult {
                memory_id: "m2".into(),
                score: 0.8,
                confidence: 0.9,
                why_retrieved: vec!["fts".into()],
                layer_signals: Default::default(),
            },
        ];
        let frame = assembler.assemble(&results, &mems);
        assert!(frame.tokens_used <= 50);
        assert!(frame.memories_included.contains(&"m1".to_string()));
    }
}
