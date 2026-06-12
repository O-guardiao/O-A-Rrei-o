use crate::envelope::MemoryEnvelope;

/// Estado do ciclo de vida de uma memória.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryState {
    Active,
    Superseded,
    Contradicted,
    Deleted,
}

/// Governança de ciclo de vida: decide se uma memória é visível e ajusta scoring.
pub struct LifecycleGovernance;

impl LifecycleGovernance {
    /// Determina o estado efetivo e o score ajustado.
    /// Se `query_historical` for true, memórias superseded são incluídas com score reduzido.
    pub fn evaluate(env: &MemoryEnvelope, query_historical: bool) -> (MemoryState, f32) {
        // Estado simulado: nesta implementação simplificada, usamos importância como proxy.
        // Memórias com importance < 0.1 são consideradas "Deleted" (soft-delete).
        let state = if env.importance < 0.1 {
            MemoryState::Deleted
        } else if env.importance < 0.3 {
            MemoryState::Superseded
        } else {
            MemoryState::Active
        };

        let adjusted_score = match state {
            MemoryState::Active => env.importance * env.confidence,
            MemoryState::Superseded if query_historical => env.importance * env.confidence * 0.5,
            MemoryState::Superseded => 0.0,
            MemoryState::Contradicted => 0.0,
            MemoryState::Deleted => 0.0,
        };

        (state, adjusted_score)
    }

    /// Heurística para detectar se a query pede conteúdo histórico.
    pub fn is_historical_query(query: &str) -> bool {
        let lowered = query.to_lowercase();
        lowered.contains("histórico")
            || lowered.contains("historico")
            || lowered.contains("passado")
            || lowered.contains("antes")
            || lowered.contains("anterior")
    }
}
