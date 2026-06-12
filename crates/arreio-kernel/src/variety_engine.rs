/// Lei da Variedade Requisitada (Ashby) — Engine para controle de variedade.
///
/// "Somente variedade pode absorver variedade."
/// S3 (Controle) atenua contexto excessivo; S4 (Inteligência) amplia estratégias.
#[derive(Debug, Clone, Default)]
pub struct VarietyEngine;

impl VarietyEngine {
    /// Atenua variedade excessiva truncando o vetor até `max_variety` elementos.
    pub fn attenuate_variety(input: Vec<String>, max_variety: usize) -> Vec<String> {
        input.into_iter().take(max_variety).collect()
    }

    /// Amplia variedade insuficiente replicando o último elemento até atingir
    /// `min_variety` elementos.
    pub fn amplify_variety(input: Vec<String>, min_variety: usize) -> Vec<String> {
        if input.is_empty() {
            return input;
        }
        let mut out = input;
        while out.len() < min_variety {
            out.push(out.last().unwrap().clone());
        }
        out
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuate_limits_to_max() {
        let input = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = VarietyEngine::attenuate_variety(input, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn amplify_expands_to_min() {
        let input = vec!["x".to_string()];
        let result = VarietyEngine::amplify_variety(input, 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result, vec!["x", "x", "x"]);
    }
}
