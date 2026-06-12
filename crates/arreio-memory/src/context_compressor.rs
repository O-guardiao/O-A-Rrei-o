//! Context Compressor Avançado — 5 fases inspirado no Hermes Agent.
//!
//! Fase 1: Prune (tool results → 1-liners, dedup, strip images, shrink JSON)
//! Fase 2: Protect Head (system prompt + primeiras N mensagens)
//! Fase 3: Protect Tail (token budget adaptativo + boundary alignment)
//! Fase 4: Summarization com LLM (template estruturado, iterative, guided)
//! Fase 5: Assembly (montagem, collision avoidance, sanitização)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Mensagem no contexto conversacional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMessage {
    pub role: ContextRole,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCallRef>>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextRole {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for ContextRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextRole::System => write!(f, "system"),
            ContextRole::User => write!(f, "user"),
            ContextRole::Assistant => write!(f, "assistant"),
            ContextRole::Tool => write!(f, "tool"),
        }
    }
}

/// Referência a uma tool call dentro de uma mensagem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRef {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Resultado da compressão.
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub messages: Vec<ContextMessage>,
    pub summary: Option<String>,
    pub removed_count: usize,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

/// Configuração do compressor.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    /// Threshold percentual da context window para disparar compressão.
    pub threshold_percent: f32,
    /// Mínimo de mensagens a preservar no head (além do system prompt).
    pub protect_first_n: usize,
    /// Budget de tokens para a tail (~20% do threshold).
    pub tail_token_percent: f32,
    /// Mínimo hard de mensagens na tail.
    pub tail_hard_minimum: usize,
    /// Budget de tokens para summarization (máximo).
    pub summary_max_tokens: usize,
    /// Budget mínimo de tokens para summarization.
    pub summary_min_tokens: usize,
    /// Se compressões consecutivas com <10% savings devem ser puladas.
    pub anti_thrash_enabled: bool,
    /// Número de compressões consecutivas ruins para ativar anti-thrash.
    pub anti_thrash_consecutive: usize,
    /// Percentual mínimo de savings para não ser considerado "thrash".
    pub anti_thrash_min_savings_percent: f32,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            threshold_percent: 0.75,
            protect_first_n: 3,
            tail_token_percent: 0.20,
            tail_hard_minimum: 3,
            summary_max_tokens: 12_000,
            summary_min_tokens: 2_000,
            anti_thrash_enabled: true,
            anti_thrash_consecutive: 2,
            anti_thrash_min_savings_percent: 0.10,
        }
    }
}

/// Motor de compressão de contexto de 5 fases.
pub struct ContextCompressor {
    config: CompressorConfig,
    /// Histórico de savings para anti-thrashing.
    last_savings: Vec<f32>,
    /// Summary anterior para updates iterativos.
    last_summary: Option<String>,
}

impl ContextCompressor {
    pub fn new(config: CompressorConfig) -> Self {
        Self {
            config,
            last_savings: Vec::new(),
            last_summary: None,
        }
    }

    /// Verifica se a compressão deve ser pulada por anti-thrashing.
    pub fn should_skip_compression(&self) -> bool {
        if !self.config.anti_thrash_enabled {
            return false;
        }
        if self.last_savings.len() < self.config.anti_thrash_consecutive {
            return false;
        }
        let recent =
            &self.last_savings[self.last_savings.len() - self.config.anti_thrash_consecutive..];
        recent
            .iter()
            .all(|&s| s < self.config.anti_thrash_min_savings_percent)
    }

    /// Registra o savings percentual de uma compressão.
    pub fn record_savings(&mut self, savings_percent: f32) {
        self.last_savings.push(savings_percent);
        if self.last_savings.len() > self.config.anti_thrash_consecutive + 1 {
            self.last_savings.remove(0);
        }
    }

    /// Reseta o estado de anti-thrashing (chamado em /new ou /compress <topic>).
    pub fn reset_anti_thrash(&mut self) {
        self.last_savings.clear();
    }

    /// Estima tokens de uma lista de mensagens (heurística chars/4).
    pub fn estimate_tokens(messages: &[ContextMessage]) -> usize {
        messages.iter().map(|m| m.content.len() / 4).sum()
    }

    /// Verifica se compressão é necessária dado uma context window.
    pub fn should_compress(&self, messages: &[ContextMessage], context_window: usize) -> bool {
        let tokens = Self::estimate_tokens(messages);
        let threshold = (context_window as f32 * self.config.threshold_percent) as usize;
        tokens > threshold
    }

    /// Fase 1: Prune — deduplica tool results, strip imagens, shrink JSON, 1-liners.
    fn phase1_prune(&self, messages: &mut Vec<ContextMessage>) {
        let mut seen_tool_results: HashMap<String, String> = HashMap::new();
        let mut _dedup_count = 0;

        for msg in messages.iter_mut() {
            if msg.role == ContextRole::Tool {
                // Deduplicação por hash do conteúdo
                let hash = format!("{:x}", md5_hash(&msg.content));
                if let Some(prev_id) = seen_tool_results.get(&hash) {
                    msg.content = format!("[duplicate of tool result {}]", prev_id);
                    _dedup_count += 1;
                } else {
                    // Strip de imagens base64
                    if msg.content.starts_with("data:image") || msg.content.len() > 100_000 {
                        msg.content = "[image content stripped]".to_string();
                    }
                    // 1-liner informativo para tool results antigos
                    if msg.content.len() > 500 {
                        let first_line = msg.content.lines().next().unwrap_or("");
                        msg.content = format!("{}... ({} chars)", first_line, msg.content.len());
                    }
                    if let Some(ref id) = msg.tool_call_id {
                        seen_tool_results.insert(hash, id.clone());
                    }
                }
            }

            // Shrink JSON args em tool_calls
            if let Some(ref mut calls) = msg.tool_calls {
                for call in calls.iter_mut() {
                    if call.arguments.len() > 200 {
                        let shrunk = shrink_json(&call.arguments);
                        call.arguments = shrunk;
                    }
                }
            }
        }
    }

    /// Fase 2: Protect Head — preserva system prompt + primeiras N mensagens.
    fn phase2_protect_head(
        &self,
        messages: &[ContextMessage],
    ) -> (Vec<ContextMessage>, Vec<ContextMessage>) {
        let system_msg = messages.iter().position(|m| m.role == ContextRole::System);
        let split_at = match system_msg {
            Some(0) => {
                1 + self
                    .config
                    .protect_first_n
                    .min(messages.len().saturating_sub(1))
            }
            _ => self.config.protect_first_n.min(messages.len()),
        };
        let head = messages[..split_at.min(messages.len())].to_vec();
        let body = messages[split_at.min(messages.len())..].to_vec();
        (head, body)
    }

    /// Fase 3: Protect Tail — acumula tokens de trás para frente até o budget.
    fn phase3_protect_tail(
        &self,
        body: &[ContextMessage],
        context_window: usize,
    ) -> (Vec<ContextMessage>, Vec<ContextMessage>) {
        let tail_budget = (context_window as f32 * self.config.tail_token_percent) as usize;
        let mut tail = Vec::new();
        let mut tail_tokens = 0;

        // Caminha de trás para frente
        for msg in body.iter().rev() {
            let msg_tokens = msg.content.len() / 4;
            // Soft ceiling: permite 1.5x o budget para evitar cortar no meio de uma mensagem
            if tail_tokens + msg_tokens > (tail_budget as f32 * 1.5) as usize
                && tail.len() >= self.config.tail_hard_minimum
            {
                break;
            }
            tail_tokens += msg_tokens;
            tail.push(msg.clone());
        }

        // Garante que a última mensagem do usuário esteja na tail
        if let Some(last_user) = body.iter().rposition(|m| m.role == ContextRole::User) {
            let last_user_msg = &body[last_user];
            if !tail.iter().any(|m| m == last_user_msg) {
                tail.push(last_user_msg.clone());
            }
        }

        tail.reverse();

        // Boundary alignment: nunca corta dentro de um par tool_call/result
        let tail_ids: std::collections::HashSet<String> =
            tail.iter().filter_map(|m| m.tool_call_id.clone()).collect();

        let middle: Vec<ContextMessage> =
            body.iter().filter(|m| !tail.contains(m)).cloned().collect();

        // Se há tool results órfãos no middle, move para tail
        let (middle_clean, extra_tail): (Vec<_>, Vec<_>) = middle.into_iter().partition(|m| {
            if m.role == ContextRole::Tool {
                if let Some(ref id) = m.tool_call_id {
                    return !tail_ids.contains(id);
                }
            }
            true
        });

        let mut final_tail = tail;
        final_tail.extend(extra_tail);
        final_tail.sort_by_key(|m| body.iter().position(|b| b == m).unwrap_or(0));

        (middle_clean, final_tail)
    }

    /// Fase 4: Summarization — gera summary estruturado do middle.
    /// Recebe uma função de summarização externa (LLM) ou usa fallback estático.
    fn phase4_summarize(
        &mut self,
        middle: &[ContextMessage],
        _context_window: usize,
        summarizer: Option<&dyn Fn(&str) -> String>,
    ) -> String {
        let content = middle
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        if let Some(sum_fn) = summarizer {
            let summary = sum_fn(&content);
            self.last_summary = Some(summary.clone());
            summary
        } else {
            // Fallback estático quando LLM não disponível
            let mut summary = String::new();
            if let Some(ref last) = self.last_summary {
                summary.push_str("## Previous Summary (Updated)\n");
                summary.push_str(last);
                summary.push('\n');
            }
            summary.push_str("## Context Summary\n");
            summary.push_str(&format!("- Messages compressed: {}\n", middle.len()));

            let mut keywords = Vec::new();
            let mut paths = Vec::new();
            for m in middle {
                let lower = m.content.to_lowercase();
                for kw in ["todo", "next", "pending", "fix", "error", "blocked", "done"] {
                    if lower.contains(kw) && !keywords.contains(&kw) {
                        keywords.push(kw);
                    }
                }
                for word in m.content.split_whitespace() {
                    if word.contains('.') && word.len() > 3 && !word.starts_with("http") {
                        let w = word.trim_matches(|c| c == '.' || c == ',' || c == ';');
                        if !paths.contains(&w) && !w.is_empty() {
                            paths.push(w);
                        }
                    }
                }
            }
            paths.truncate(10);

            if !keywords.is_empty() {
                summary.push_str(&format!("- Keywords: {}\n", keywords.join(", ")));
            }
            if !paths.is_empty() {
                summary.push_str(&format!("- Files: {}\n", paths.join(", ")));
            }

            self.last_summary = Some(summary.clone());
            summary
        }
    }

    /// Fase 5: Assembly — monta a lista final comprimida.
    fn phase5_assemble(
        &self,
        head: Vec<ContextMessage>,
        summary: String,
        tail: Vec<ContextMessage>,
    ) -> Vec<ContextMessage> {
        let mut result = head;

        // Adiciona mensagem de summary
        let summary_msg = ContextMessage {
            role: ContextRole::User,
            content: format!("[CONTEXT SUMMARY]\n{}", summary),
            tool_calls: None,
            tool_call_id: None,
        };

        // Collision avoidance: evita consecutivos same-role
        if let Some(last) = result.last() {
            if last.role == summary_msg.role {
                // Mescla no último head message
                if let Some(last_mut) = result.last_mut() {
                    last_mut.content.push_str("\n\n");
                    last_mut.content.push_str(&summary_msg.content);
                }
            } else {
                result.push(summary_msg);
            }
        } else {
            result.push(summary_msg);
        }

        // Adiciona nota de compressão no system prompt se existir
        if let Some(first) = result.first_mut() {
            if first.role == ContextRole::System {
                first.content.push_str("\n[Note: context has been compressed. Previous messages are summarized above.]");
            }
        }

        result.extend(tail);

        // Sanitização: remove tool results órfãos
        let tool_call_ids: std::collections::HashSet<String> = result
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|c| c.id.clone())
            .collect();

        result.retain(|m| {
            if m.role == ContextRole::Tool {
                if let Some(ref id) = m.tool_call_id {
                    return tool_call_ids.contains(id);
                }
            }
            true
        });

        result
    }

    /// Comprime mensagens usando as 5 fases.
    pub fn compress(
        &mut self,
        messages: &[ContextMessage],
        context_window: usize,
        summarizer: Option<&dyn Fn(&str) -> String>,
    ) -> CompressionResult {
        let original_tokens = Self::estimate_tokens(messages);
        let original_count = messages.len();

        if messages.is_empty() {
            return CompressionResult {
                messages: vec![],
                summary: None,
                removed_count: 0,
                original_tokens,
                compressed_tokens: 0,
            };
        }

        // Fase 1: Prune
        let mut pruned = messages.to_vec();
        self.phase1_prune(&mut pruned);

        // Fase 2: Protect Head
        let (head, body) = self.phase2_protect_head(&pruned);

        if body.is_empty() {
            return CompressionResult {
                messages: head,
                summary: None,
                removed_count: 0,
                original_tokens,
                compressed_tokens: original_tokens,
            };
        }

        // Fase 3: Protect Tail
        let (middle, tail) = self.phase3_protect_tail(&body, context_window);

        // Fase 4: Summarize
        let summary = self.phase4_summarize(&middle, context_window, summarizer);

        // Fase 5: Assemble
        let compressed = self.phase5_assemble(head, summary.clone(), tail);

        let compressed_tokens = Self::estimate_tokens(&compressed);
        let removed_count = original_count.saturating_sub(compressed.len());

        CompressionResult {
            messages: compressed,
            summary: Some(summary),
            removed_count,
            original_tokens,
            compressed_tokens,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn md5_hash(s: &str) -> u64 {
    use std::hash::{DefaultHasher, Hasher};
    let mut h = DefaultHasher::new();
    h.write(s.as_bytes());
    h.finish()
}

/// Compacta JSON mantendo estrutura mas truncando strings longas.
fn shrink_json(json_str: &str) -> String {
    // Heurística simples: trunca strings longas dentro do JSON
    let mut result = String::new();
    let mut in_string = false;
    let mut string_start = 0;

    for (i, c) in json_str.char_indices() {
        if c == '"' {
            if in_string {
                let str_content = &json_str[string_start..i];
                if str_content.len() > 100 {
                    result.push_str(&str_content[..50]);
                    result.push_str("...[truncated]");
                } else {
                    result.push_str(str_content);
                }
                result.push('"');
                in_string = false;
            } else {
                result.push('"');
                in_string = true;
                string_start = i + 1;
            }
        } else if !in_string {
            result.push(c);
        }
    }

    if result.len() > json_str.len() / 2 {
        result
    } else {
        format!(
            "{}...[truncated {} chars]",
            &json_str[..100],
            json_str.len()
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msgs(n: usize) -> Vec<ContextMessage> {
        (0..n)
            .map(|i| ContextMessage {
                role: if i == 0 {
                    ContextRole::System
                } else if i % 2 == 1 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                content: format!("message content number {} with some text", i),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect()
    }

    fn make_msgs_with_tools() -> Vec<ContextMessage> {
        vec![
            ContextMessage {
                role: ContextRole::System,
                content: "You are an assistant".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::User,
                content: "Run tests".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Assistant,
                content: "I'll run the tests".to_string(),
                tool_calls: Some(vec![ToolCallRef {
                    id: "call_1".to_string(),
                    name: "terminal".to_string(),
                    arguments: r#"{"command": "cargo test", "very_long_argument": "..."}"#
                        .to_string(),
                }]),
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Tool,
                content: "test result 1\ntest result 2\ntest result 3\n...".repeat(50),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
            ContextMessage {
                role: ContextRole::User,
                content: "What about file.rs?".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Assistant,
                content: "Looking at file.rs".to_string(),
                tool_calls: Some(vec![ToolCallRef {
                    id: "call_2".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "file.rs"}"#.to_string(),
                }]),
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Tool,
                content: "test result 1\ntest result 2\ntest result 3\n...".repeat(50),
                tool_calls: None,
                tool_call_id: Some("call_2".to_string()),
            },
        ]
    }

    #[test]
    fn no_compression_needed_short() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs(3);
        let result = comp.compress(&msgs, 10_000, None);
        assert_eq!(result.removed_count, 0);
        assert!(result.summary.is_none() || result.messages.len() <= 3);
    }

    #[test]
    fn compress_long_context() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        // Mensagens grandes para garantir que atinge threshold
        let msgs: Vec<ContextMessage> = (0..20)
            .map(|i| ContextMessage {
                role: if i == 0 {
                    ContextRole::System
                } else if i % 2 == 1 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                content: "a".repeat(500), // ~125 tokens cada
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        // 20 * 125 = 2500 tokens; threshold 75% de 2000 = 1500 → deve comprimir
        let result = comp.compress(&msgs, 2_000, None);
        assert!(
            result.removed_count > 0,
            "deve remover mensagens: original={} compressed={}",
            result.original_tokens,
            result.compressed_tokens
        );
        assert!(result.summary.is_some());
    }

    #[test]
    fn protects_head() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs(20);
        let result = comp.compress(&msgs, 2_000, None);
        // System prompt deve estar preservado
        assert_eq!(result.messages.first().unwrap().role, ContextRole::System);
    }

    #[test]
    fn protects_tail_last_user() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs(20);
        let result = comp.compress(&msgs, 2_000, None);
        // Última mensagem do usuário deve estar na tail
        let has_last_user = result
            .messages
            .iter()
            .any(|m| m.role == ContextRole::User && m.content.contains("number 19"));
        assert!(
            has_last_user,
            "última mensagem do usuário deve ser preservada"
        );
    }

    #[test]
    fn tool_deduplication() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let mut msgs = make_msgs_with_tools();
        // Duplica o tool result
        let dup = msgs[3].clone();
        msgs.push(dup);
        let result = comp.compress(&msgs, 2_000, None);
        let dup_count = result
            .messages
            .iter()
            .filter(|m| m.content.contains("[duplicate of tool result"))
            .count();
        assert!(dup_count >= 1, "deve detectar duplicatas");
    }

    #[test]
    fn shrinks_json_args() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs_with_tools();
        let result = comp.compress(&msgs, 2_000, None);
        let has_tool = result
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .any(|c| c.arguments.len() < 200);
        assert!(
            has_tool,
            "argumentos JSON longos devem ser encurtados: encontrado {}",
            result
                .messages
                .iter()
                .filter_map(|m| m.tool_calls.as_ref())
                .flatten()
                .map(|c| c.arguments.len())
                .max()
                .unwrap_or(0)
        );
    }

    #[test]
    fn anti_thrash_skips() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        comp.record_savings(0.05);
        comp.record_savings(0.03);
        assert!(comp.should_skip_compression());
    }

    #[test]
    fn anti_thrash_allows_after_good_savings() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        comp.record_savings(0.05);
        comp.record_savings(0.50);
        assert!(!comp.should_skip_compression());
    }

    #[test]
    fn boundary_alignment_no_orphan_tools() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs_with_tools();
        let result = comp.compress(&msgs, 2_000, None);
        // Se há tool result, deve haver tool_call correspondente
        let tool_results: Vec<_> = result
            .messages
            .iter()
            .filter(|m| m.role == ContextRole::Tool)
            .collect();
        let tool_calls: std::collections::HashSet<String> = result
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|c| c.id.clone())
            .collect();
        for tr in tool_results {
            if let Some(ref id) = tr.tool_call_id {
                assert!(
                    tool_calls.contains(id),
                    "tool result {} sem tool_call correspondente",
                    id
                );
            }
        }
    }

    #[test]
    fn collision_avoidance_same_role() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = vec![
            ContextMessage {
                role: ContextRole::User,
                content: "head".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Assistant,
                content: "body1".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::Assistant,
                content: "body2".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
            ContextMessage {
                role: ContextRole::User,
                content: "tail".to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let result = comp.compress(&msgs, 100, None);
        // Verifica que não há dois users consecutivos
        for window in result.messages.windows(2) {
            if window[0].role == ContextRole::User && window[1].role == ContextRole::User {
                // É permitido se o segundo é summary mesclado
                assert!(
                    window[1].content.contains("[CONTEXT SUMMARY]")
                        || window[0].content.contains("[CONTEXT SUMMARY]")
                );
            }
        }
    }

    #[test]
    fn summarize_includes_keywords() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        // Gera mensagens grandes suficientes para forçar compressão
        let mut msgs: Vec<ContextMessage> = (0..10)
            .map(|i| ContextMessage {
                role: if i == 0 {
                    ContextRole::System
                } else if i % 2 == 1 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                content: "a".repeat(500),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        msgs[3].content = "fix the bug in main.rs".repeat(20);
        msgs[5].content = "todo: check error in lib.rs".repeat(20);
        let result = comp.compress(&msgs, 2_000, None);
        let summary_text = result.summary.unwrap_or_default();
        assert!(
            summary_text.contains("fix")
                || summary_text.contains("todo")
                || summary_text.contains("main.rs"),
            "summary deve conter keywords ou paths: {}",
            summary_text
        );
    }

    #[test]
    fn custom_summarizer_called() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs = make_msgs(10);
        let custom = |_: &str| "CUSTOM SUMMARY".to_string();
        let result = comp.compress(&msgs, 500, Some(&custom));
        assert!(result.summary.unwrap().contains("CUSTOM SUMMARY"));
    }

    #[test]
    fn iterative_summary_preserved() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        let msgs1 = make_msgs(10);
        comp.compress(&msgs1, 500, None);
        assert!(comp.last_summary.is_some());

        let msgs2 = make_msgs(5);
        let result2 = comp.compress(&msgs2, 500, None);
        let summary2 = result2.summary.unwrap();
        assert!(
            summary2.contains("Previous Summary"),
            "deve preservar summary anterior: {}",
            summary2
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut comp = ContextCompressor::new(CompressorConfig::default());
        comp.record_savings(0.05);
        comp.record_savings(0.03);
        comp.reset_anti_thrash();
        assert!(!comp.should_skip_compression());
    }

    #[test]
    fn image_content_stripped() {
        let comp = ContextCompressor::new(CompressorConfig::default());
        let mut msgs: Vec<ContextMessage> = (0..10)
            .map(|i| ContextMessage {
                role: if i == 0 {
                    ContextRole::System
                } else if i % 2 == 1 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                content: "a".repeat(500),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        // Tool message com imagem base64
        msgs[5].role = ContextRole::Tool;
        msgs[5].tool_call_id = Some("img_1".to_string());
        msgs[5].content = "data:image/png;base64,iVBORw0KGgo".repeat(5000); // >100KB
        let mut pruned = msgs.clone();
        comp.phase1_prune(&mut pruned);
        let has_stripped = pruned
            .iter()
            .any(|m| m.content.contains("[image content stripped]"));
        assert!(
            has_stripped,
            "imagem base64 deve ser stripped em tool results"
        );
    }
}
