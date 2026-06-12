use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Regra de produção ACT-R/SOAR: padrão → diagnóstico → correção.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProductionRule {
    pub id: String,
    pub pattern: String,
    pub diagnosis: String,
    pub correction: String,
    pub success_rate: f64,
}

/// Memória procedural armazenada no Blackboard.
pub struct ProceduralMemory {
    blackboard: Blackboard,
}

impl ProceduralMemory {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Armazena uma regra de produção.
    pub fn learn_rule(&self, rule: ProductionRule) -> Result<()> {
        let key = format!("{}:rule", rule.id);
        self.blackboard
            .put_tuple("memory:procedural", &key, serde_json::to_value(&rule)?)
    }

    /// Encontra a primeira regra cujo `pattern` contenha a substring `error_pattern`
    /// ou vice-versa.
    pub fn match_rule(&self, error_pattern: &str) -> Result<Option<ProductionRule>> {
        let all = self.blackboard.search_tuples("memory:procedural", "");
        let err_lower = error_pattern.to_lowercase();
        for (_, v) in all {
            if let Ok(rule) = serde_json::from_value::<ProductionRule>(v) {
                let pat_lower = rule.pattern.to_lowercase();
                if pat_lower.contains(&err_lower) || err_lower.contains(&pat_lower) {
                    return Ok(Some(rule));
                }
            }
        }
        Ok(None)
    }

    /// Reforça ou pune uma regra existente, ajustando `success_rate`.
    pub fn reinforce_rule(&self, rule_id: &str, success: bool) -> Result<()> {
        let key = format!("{}:rule", rule_id);
        match self.blackboard.get_tuple("memory:procedural", &key) {
            Some(v) => {
                let mut rule: ProductionRule = serde_json::from_value(v)?;
                if success {
                    rule.success_rate = ((rule.success_rate * 9.0) + 1.0) / 10.0;
                } else {
                    rule.success_rate = (rule.success_rate * 9.0) / 10.0;
                };
                self.blackboard
                    .put_tuple("memory:procedural", &key, serde_json::to_value(&rule)?)
            }
            None => Ok(()),
        }
    }
}

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

    fn make_rule(id: &str, pattern: &str, diagnosis: &str, correction: &str) -> ProductionRule {
        ProductionRule {
            id: id.into(),
            pattern: pattern.into(),
            diagnosis: diagnosis.into(),
            correction: correction.into(),
            success_rate: 0.5,
        }
    }

    #[test]
    fn procedural_memory_aprende_regra() {
        let bb = temp_bb();
        let mem = ProceduralMemory::new(bb);
        let rule = make_rule("r1", "panic", "stack overflow", "aumentar heap");
        mem.learn_rule(rule.clone()).unwrap();

        let loaded = mem.match_rule("panic").unwrap().unwrap();
        assert_eq!(loaded.id, "r1");
    }

    #[test]
    fn procedural_memory_match_por_padrao() {
        let bb = temp_bb();
        let mem = ProceduralMemory::new(bb);
        mem.learn_rule(make_rule(
            "r1",
            "borrow checker error",
            "lifetime inválido",
            "adicionar 'a",
        ))
        .unwrap();

        let m = mem.match_rule("borrow checker").unwrap();
        assert!(m.is_some());
        assert_eq!(m.unwrap().id, "r1");
    }

    #[test]
    fn procedural_memory_reforca_aumenta_success_rate() {
        let bb = temp_bb();
        let mem = ProceduralMemory::new(bb);
        mem.learn_rule(make_rule("r1", "x", "y", "z")).unwrap();

        mem.reinforce_rule("r1", true).unwrap();
        let r = mem.match_rule("x").unwrap().unwrap();
        assert!(r.success_rate > 0.5);

        mem.reinforce_rule("r1", false).unwrap();
        let r2 = mem.match_rule("x").unwrap().unwrap();
        assert!(r2.success_rate < r.success_rate);
    }

    #[test]
    fn match_retorna_none_quando_nao_encontra() {
        let bb = temp_bb();
        let mem = ProceduralMemory::new(bb);
        mem.learn_rule(make_rule("r1", "patternA", "d", "c"))
            .unwrap();
        let m = mem.match_rule("inexistente").unwrap();
        assert!(m.is_none());
    }

    #[test]
    fn procedural_memory_integracao_blackboard() {
        let bb = temp_bb();
        let mem = ProceduralMemory::new(bb.clone());
        mem.learn_rule(make_rule(
            "bb_rule",
            "erro de compilação",
            "falta ponto-e-vírgula",
            "inserir ;",
        ))
        .unwrap();

        let val = bb.get_tuple("memory:procedural", "bb_rule:rule").unwrap();
        let rule: ProductionRule = serde_json::from_value(val).unwrap();
        assert_eq!(rule.id, "bb_rule");
    }
}
