/// Compressão heurística de contexto para economia de tokens.
/// Inspirado no Claw-Code: preserva boundary ToolUse/ToolResult, mantém mensagens recentes,
/// extrai keywords de pending work, e sumariza o restante.
pub struct ContextCompressor {
    /// Quantas mensagens recentes preservar sem tocar.
    preserve_recent: usize,
    /// Target de tokens (heurística chars/4).
    target_tokens: usize,
}

impl ContextCompressor {
    pub fn new(preserve_recent: usize, target_tokens: usize) -> Self {
        Self {
            preserve_recent,
            target_tokens,
        }
    }

    /// Comprime uma lista de mensagens (system + user + assistant + tool).
    /// Retorna texto comprimido respeitando target_tokens.
    pub fn compress(&self, messages: &[ChatMessage]) -> String {
        if messages.len() <= self.preserve_recent {
            return serialize_messages(messages);
        }

        let (old, recent) = messages.split_at(messages.len() - self.preserve_recent);
        let target_chars = self.target_tokens * 4;

        // Sumarização do contexto antigo
        let summary = summarize_old(old);
        let recent_text = serialize_messages(recent);

        let combined = format!(
            "## Contexto Anterior (Resumido)\n{}\n\n## Contexto Recente\n{}",
            summary, recent_text
        );

        if combined.len() > target_chars {
            // Trunca sumário se ainda excede
            let trunc_summary = &summary[..summary.len().min(target_chars / 2)];
            format!(
                "## Contexto Anterior (Resumido)\n{}...\n\n## Contexto Recente\n{}",
                trunc_summary, recent_text
            )
        } else {
            combined
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

fn serialize_messages(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn summarize_old(messages: &[ChatMessage]) -> String {
    // Heurística: extrai keywords de pending work e paths de arquivo
    let mut keywords = Vec::new();
    let mut paths = Vec::new();
    for m in messages {
        let lower = m.content.to_lowercase();
        for kw in [
            "todo",
            "next",
            "pending",
            "follow up",
            "remaining",
            "fix",
            "error",
        ] {
            if lower.contains(kw) {
                keywords.push(kw.to_string());
            }
        }
        // Extrai paths com extensões
        for word in m.content.split_whitespace() {
            if word.contains('.') && !word.starts_with("http") {
                paths.push(word.to_string());
            }
        }
    }
    keywords.dedup();
    paths.dedup();
    paths.truncate(10);

    let mut summary = String::new();
    if !keywords.is_empty() {
        summary.push_str(&format!("Keywords: {}\n", keywords.join(", ")));
    }
    if !paths.is_empty() {
        summary.push_str(&format!("Arquivos: {}\n", paths.join(", ")));
    }
    summary.push_str(&format!("Mensagens sumarizadas: {}\n", messages.len()));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(n: usize) -> Vec<ChatMessage> {
        (0..n)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                content: format!("msg {}", i),
            })
            .collect()
    }

    #[test]
    fn compress_mantem_recentes() {
        let comp = ContextCompressor::new(2, 100);
        let compressed = comp.compress(&msgs(5));
        assert!(compressed.contains("msg 3"));
        assert!(compressed.contains("msg 4"));
        assert!(compressed.contains("Contexto Anterior (Resumido)"));
    }

    #[test]
    fn nao_compress_curto() {
        let comp = ContextCompressor::new(4, 1000);
        let compressed = comp.compress(&msgs(3));
        assert!(compressed.contains("msg 0"));
        assert!(!compressed.contains("Resumido"));
    }
}
