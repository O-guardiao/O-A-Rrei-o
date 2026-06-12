use serde::{Deserialize, Serialize};

/// Tipos de memória canônicos, inspirados no Agent-Memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    Episodic,   // algo que aconteceu
    Semantic,   // fato durável
    Procedural, // modo de fazer / workflow
    Error,      // erro ocorrido
    Solution,   // resolução de erro
    Decision,   // decisão tomada
    Preference, // preferência do usuário
    Artifact,   // arquivo ou chunk de código
}

/// Escopo hierárquico da memória.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub agent_id: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
}

/// Referência a uma modalidade de conteúdo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalityRef {
    pub modality_type: String, // "text", "structured", "file_ref"
    pub content: String,
}

/// Envelope multimodal de memória — a tupla canônica do motor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEnvelope {
    pub id: String, // UUID v4
    pub scope: Scope,
    pub memory_type: MemoryType,
    pub modalities: Vec<ModalityRef>,
    pub importance: f32, // 0.0 – 1.0
    pub confidence: f32, // 0.0 – 1.0
    pub entities: Vec<String>,
    pub tags: Vec<String>,
    pub content_hash: String, // SHA-256 simplificado
    pub created_at: u64,      // timestamp UNIX
}

impl MemoryEnvelope {
    /// Conteúdo textual primário (primeira modalidade do tipo "text").
    pub fn primary_text(&self) -> Option<&str> {
        self.modalities
            .iter()
            .find(|m| m.modality_type == "text")
            .map(|m| m.content.as_str())
    }

    /// Representação compacta para indexing e display.
    pub fn summary(&self) -> String {
        let text = self.primary_text().unwrap_or("");
        let truncated = if text.len() > 120 {
            format!("{}...", &text[..120])
        } else {
            text.to_string()
        };
        format!(
            "[{}] {} | imp={:.2} conf={:.2} | {}",
            self.id,
            format!("{:?}", self.memory_type),
            self.importance,
            self.confidence,
            truncated
        )
    }
}
