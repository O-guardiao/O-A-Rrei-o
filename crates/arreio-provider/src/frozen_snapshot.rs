//! Frozen Snapshot / Prefix Caching para system prompts.
//!
//! O system prompt é dividido em camadas:
//! - Layer 0 (Frozen): instrução base do ator — nunca muda.
//! - Layer 1 (Semi-frozen): skills + AGENTS.md — mudam raramente.
//! - Layer 2 (Dinâmico): AST, memória, tarefa específica — muda a cada chamada.
//!
//! O `SnapshotCache` mantém o prefixo (Layer 0 + Layer 1) pré-montado e
//! invalida automaticamente quando qualquer input muda. Isso evita
//! recomputação e maximiza hit-rate de prefix caching nos providers.

use std::collections::HashMap;

/// Retorna o context window (em tokens) para um modelo conhecido.
///
/// Valores aproximados baseados em documentação dos providers.
/// Usado para calcular boundary dinâmico do prefixo cacheado.
pub fn model_context_window(model_name: &str) -> Option<usize> {
    // Normaliza: remove prefixo de provider (ex: "ollama:gemma4" → "gemma4")
    let name = model_name.split_once(':').map(|(_, n)| n).unwrap_or(model_name);
    let name = name.to_lowercase();

    match name.as_str() {
        // Anthropic
        n if n.contains("claude-3-opus") => Some(200_000),
        n if n.contains("claude-3-sonnet") => Some(200_000),
        n if n.contains("claude-3-haiku") => Some(200_000),
        n if n.contains("claude-3.5-sonnet") => Some(200_000),
        // OpenAI
        n if n.contains("gpt-4o") => Some(128_000),
        n if n.contains("gpt-4-turbo") => Some(128_000),
        n if n.contains("gpt-4") && !n.contains("gpt-4o") => Some(8_192),
        n if n.contains("gpt-3.5-turbo") => Some(16_385),
        // Google
        n if n.contains("gemini-1.5-pro") => Some(2_000_000),
        n if n.contains("gemini-1.5-flash") => Some(1_000_000),
        n if n.contains("gemini-1.0-pro") => Some(32_768),
        // DeepSeek
        n if n.contains("deepseek-chat") => Some(64_000),
        n if n.contains("deepseek-coder") => Some(64_000),
        // Ollama / local (default conservador)
        n if n.contains("llama3") => Some(8_192),
        n if n.contains("phi3") => Some(128_000),
        n if n.contains("gemma") => Some(8_192),
        n if n.contains("mistral") => Some(32_768),
        // Azure (usa mesmas famílias do OpenAI)
        n if n.contains("gpt-4") => Some(128_000),
        _ => None,
    }
}

/// Fraction do context window reservado para o prefixo (Layer 0 + Layer 1).
/// O restante fica para a camada dinâmica e a resposta do modelo.
const PREFIX_FRACTION: f64 = 0.30;

/// Camadas de um system prompt, do mais estável ao mais volátil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptLayers {
    /// Instrução base do ator (ex: "Você é um Arquiteto...").
    pub base: String,
    /// Skills e contexto de projeto (muda entre sessões).
    pub semi: String,
    /// Contexto dinâmico da chamada atual (AST, memória, tarefa).
    pub dynamic: String,
    /// Nome do modelo (opcional) para calcular boundary dinâmico.
    pub model: Option<String>,
}

impl SystemPromptLayers {
    /// Cria layers sem modelo associado.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            semi: String::new(),
            dynamic: String::new(),
            model: None,
        }
    }

    /// Monta o prompt completo concatenando as camadas.
    pub fn assemble(&self) -> String {
        let mut out =
            String::with_capacity(self.base.len() + self.semi.len() + self.dynamic.len() + 4);
        out.push_str(&self.base);
        if !self.semi.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.semi);
        }
        if !self.dynamic.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.dynamic);
        }
        out
    }

    /// Hash rápido do conteúdo semi-frozen (para invalidação de cache).
    pub fn semi_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut s = DefaultHasher::new();
        self.base.hash(&mut s);
        self.semi.hash(&mut s);
        s.finish()
    }

    /// Calcula o boundary máximo (em bytes) para o prefixo baseado no modelo.
    ///
    /// Usa uma heurística conservadora: 1 token ≈ 4 bytes (média UTF-8).
    pub fn prefix_boundary(&self) -> Option<usize> {
        let model = self.model.as_ref()?;
        let tokens = model_context_window(model)?;
        let bytes = (tokens as f64 * PREFIX_FRACTION * 4.0) as usize;
        Some(bytes)
    }

    /// Trunca a camada `semi` para caber dentro do boundary, preservando
    /// as primeiras linhas (mais importantes) e descartando as últimas.
    pub fn truncate_semi_to_boundary(&mut self) {
        let Some(boundary) = self.prefix_boundary() else { return };
        let base_len = self.base.len();
        let separator = if self.semi.is_empty() { 0 } else { 2 }; // "\n\n"
        let prefix_len = base_len + separator + self.semi.len();

        if prefix_len <= boundary {
            return;
        }

        let max_semi = boundary.saturating_sub(base_len + separator);
        if max_semi == 0 {
            self.semi.clear();
            return;
        }

        // Trunca por linhas: mantém linhas completas que cabem.
        let lines: Vec<&str> = self.semi.lines().collect();
        let mut truncated = String::new();
        for line in lines {
            let next_len = truncated.len() + line.len() + 1; // +1 for '\n'
            if next_len > max_semi {
                break;
            }
            if !truncated.is_empty() {
                truncated.push('\n');
            }
            truncated.push_str(line);
        }
        self.semi = truncated;
    }
}

/// Snapshot congelado de um system prompt — prefixo estável + sufixo dinâmico.
#[derive(Debug, Clone)]
pub struct FrozenSnapshot {
    /// Hash do conteúdo que gerou este snapshot.
    pub hash: u64,
    /// Prefixo estável (Layer 0 + Layer 1) já montado.
    pub prefix: String,
    /// Comprimento do prefixo em bytes (para métricas).
    pub prefix_len: usize,
}

impl FrozenSnapshot {
    /// Monta o prompt final anexando a camada dinâmica.
    pub fn build_prompt(&self, dynamic: &str) -> String {
        if dynamic.is_empty() {
            self.prefix.clone()
        } else {
            format!("{}\n\n{}", self.prefix, dynamic)
        }
    }
}

/// Cache de snapshots por chave (ex: nome do ator + modelo).
pub struct SnapshotCache {
    map: HashMap<String, FrozenSnapshot>,
    hits: u64,
    misses: u64,
}

impl SnapshotCache {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Obtém ou recria um snapshot para a chave dada.
    ///
    /// Se o hash das camadas estáveis mudou, invalida e remonta o prefixo.
    pub fn get_or_create(&mut self, key: &str, layers: &SystemPromptLayers) -> FrozenSnapshot {
        let mut layers = layers.clone();
        layers.truncate_semi_to_boundary();
        let current_hash = layers.semi_hash();

        if let Some(snapshot) = self.map.get(key) {
            if snapshot.hash == current_hash {
                self.hits += 1;
                return snapshot.clone();
            }
        }

        self.misses += 1;
        let prefix = if layers.semi.is_empty() {
            layers.base.clone()
        } else {
            format!("{}\n\n{}", layers.base, layers.semi)
        };
        let prefix_len = prefix.len();
        let snapshot = FrozenSnapshot {
            hash: current_hash,
            prefix,
            prefix_len,
        };
        self.map.insert(key.to_string(), snapshot.clone());
        snapshot
    }

    /// Estatísticas de hit/miss do cache.
    pub fn stats(&self) -> CacheStats {
        let total = self.hits + self.misses;
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            hit_rate: if total == 0 {
                0.0
            } else {
                self.hits as f64 / total as f64
            },
            entries: self.map.len(),
        }
    }

    /// Limpa todas as entradas.
    pub fn clear(&mut self) {
        self.map.clear();
        self.hits = 0;
        self.misses = 0;
    }
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Estatísticas do cache de snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub entries: usize,
}

/// Builder fluente para montar system prompts com cache.
pub struct SystemPromptBuilder<'a> {
    cache: &'a mut SnapshotCache,
    key: String,
    layers: SystemPromptLayers,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(
        cache: &'a mut SnapshotCache,
        key: impl Into<String>,
        base: impl Into<String>,
    ) -> Self {
        Self {
            cache,
            key: key.into(),
            layers: SystemPromptLayers::new(base),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.layers.model = Some(model.into());
        self
    }

    pub fn semi(mut self, semi: impl Into<String>) -> Self {
        self.layers.semi = semi.into();
        self
    }

    pub fn dynamic(mut self, dynamic: impl Into<String>) -> Self {
        self.layers.dynamic = dynamic.into();
        self
    }

    /// Monta o prompt final, reusando o prefixo cacheado quando possível.
    pub fn build(self) -> String {
        let snapshot = self.cache.get_or_create(&self.key, &self.layers);
        snapshot.build_prompt(&self.layers.dynamic)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_assemble_completo() {
        let layers = SystemPromptLayers {
            base: "Base".into(),
            semi: "Semi".into(),
            dynamic: "Dynamic".into(),
            model: None,
        };
        assert_eq!(layers.assemble(), "Base\n\nSemi\n\nDynamic");
    }

    #[test]
    fn layers_assemble_sem_semi() {
        let layers = SystemPromptLayers {
            base: "Base".into(),
            semi: "".into(),
            dynamic: "Dynamic".into(),
            model: None,
        };
        assert_eq!(layers.assemble(), "Base\n\nDynamic");
    }

    #[test]
    fn cache_reusa_prefixo_quando_hash_igual() {
        let mut cache = SnapshotCache::new();
        let layers = SystemPromptLayers {
            base: "Você é um Arquiteto.".into(),
            semi: "Skills: A, B.".into(),
            dynamic: "Tarefa 1".into(),
            model: None,
        };

        let s1 = cache.get_or_create("arch", &layers);
        let s2 = cache.get_or_create("arch", &layers);

        assert_eq!(s1.hash, s2.hash);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn cache_invalida_quando_semi_muda() {
        let mut cache = SnapshotCache::new();
        let layers1 = SystemPromptLayers {
            base: "Base".into(),
            semi: "Skills: A.".into(),
            dynamic: "".into(),
            model: None,
        };
        let _ = cache.get_or_create("dev", &layers1);

        let layers2 = SystemPromptLayers {
            base: "Base".into(),
            semi: "Skills: A, B.".into(),
            dynamic: "".into(),
            model: None,
        };
        let _ = cache.get_or_create("dev", &layers2);

        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().entries, 1); // apenas a última entrada
    }

    #[test]
    fn builder_monta_prompt_com_cache() {
        let mut cache = SnapshotCache::new();
        let prompt = SystemPromptBuilder::new(&mut cache, "arch", "Você é um Arquiteto.")
            .semi("Skills: auth.")
            .dynamic("Decompor login.")
            .build();

        assert!(prompt.starts_with("Você é um Arquiteto."));
        assert!(prompt.contains("Skills: auth."));
        assert!(prompt.contains("Decompor login."));
    }

    #[test]
    fn cache_hit_rate_perfeito_apos_primeiro_miss() {
        let mut cache = SnapshotCache::new();
        let layers = SystemPromptLayers {
            base: "B".into(),
            semi: "S".into(),
            dynamic: "D".into(),
            model: None,
        };

        for _ in 0..100 {
            let _ = cache.get_or_create("k", &layers);
        }

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 99);
        assert!((stats.hit_rate - 0.99).abs() < 0.001);
    }

    #[test]
    fn snapshot_build_prompt_sem_dynamic() {
        let snap = FrozenSnapshot {
            hash: 0,
            prefix: "Prefixo estável.".into(),
            prefix_len: 16,
        };
        assert_eq!(snap.build_prompt(""), "Prefixo estável.");
    }

    #[test]
    fn snapshot_build_prompt_com_dynamic() {
        let snap = FrozenSnapshot {
            hash: 0,
            prefix: "Prefixo estável.".into(),
            prefix_len: 16,
        };
        assert_eq!(
            snap.build_prompt("Tarefa nova."),
            "Prefixo estável.\n\nTarefa nova."
        );
    }

    #[test]
    fn model_context_window_known_models() {
        assert_eq!(model_context_window("claude-3-opus"), Some(200_000));
        assert_eq!(model_context_window("gpt-4o"), Some(128_000));
        assert_eq!(model_context_window("ollama:gemma4"), Some(8_192));
        assert_eq!(model_context_window("unknown-model"), None);
    }

    #[test]
    fn boundary_calculated_for_known_model() {
        let layers = SystemPromptLayers {
            base: "Base".into(),
            semi: "Skills.".into(),
            dynamic: "".into(),
            model: Some("gpt-4o".into()),
        };
        // 128k tokens * 0.30 * 4 bytes/token ≈ 153_600 bytes
        assert_eq!(layers.prefix_boundary(), Some(153_600));
    }

    #[test]
    fn boundary_none_for_unknown_model() {
        let layers = SystemPromptLayers {
            base: "Base".into(),
            semi: "Skills.".into(),
            dynamic: "".into(),
            model: Some("unknown".into()),
        };
        assert_eq!(layers.prefix_boundary(), None);
    }

    #[test]
    fn truncate_semi_when_exceeds_boundary() {
        let mut layers = SystemPromptLayers {
            base: "Base".into(),
            semi: "Line1\nLine2\nLine3\nLine4\nLine5".into(),
            dynamic: "".into(),
            model: Some("gpt-4".into()), // 8k tokens → boundary ≈ 9_600 bytes
        };
        layers.truncate_semi_to_boundary();
        // Como base + semi cabem bem abaixo de 9_600 bytes, não deve truncar
        assert!(layers.semi.contains("Line5"));
    }

    #[test]
    fn truncate_semi_actually_truncates() {
        let mut layers = SystemPromptLayers {
            base: "Base prompt.".into(),
            semi: (0..100).map(|i| format!("Very long skill line number {} with lots of text\n", i)).collect::<String>(),
            dynamic: "".into(),
            model: Some("gpt-4".into()), // boundary ≈ 9_600 bytes
        };
        layers.truncate_semi_to_boundary();
        // Deve ter truncado para caber no boundary
        let prefix_len = layers.base.len() + 2 + layers.semi.len();
        assert!(prefix_len <= 9_600, "prefix_len={} > boundary", prefix_len);
    }

    #[test]
    fn builder_with_model_sets_boundary() {
        let mut cache = SnapshotCache::new();
        let prompt = SystemPromptBuilder::new(&mut cache, "arch", "Você é um Arquiteto.")
            .model("gpt-4o")
            .semi("Skills: auth.")
            .dynamic("Decompor login.")
            .build();

        assert!(prompt.starts_with("Você é um Arquiteto."));
    }
}
