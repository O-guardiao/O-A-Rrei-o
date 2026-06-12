//! NodeScore — priorização dinâmica de nós DAG (PVC-Q3.1).
//!
//! Cada nó pronto pode receber um score composto por cinco dimensões
//! (urgência, importância, deadline, risco, custo). O score é calculado
//! deterministicamente pelo harness — nunca por LLM — e vive como tupla
//! no Blackboard (`dag::score:{node_id}`), preservando o formato de
//! `DagNode` para todos os bridges e consumidores existentes.
//!
//! Política de composição (pesos fixos, documentados no ADR-0011):
//! - urgência 30% — quão cedo o resultado é necessário
//! - importância 30% — impacto no objetivo do plano
//! - pressão de deadline 20% — cresce linearmente nos últimos 7 dias
//! - risco 10% — nós arriscados rodam cedo (fail-fast)
//! - custo 10% — invertido: nós baratos rodam antes (quick wins)

use serde::{Deserialize, Serialize};

/// Horizonte de pressão de deadline: 7 dias em segundos.
/// Deadlines além do horizonte exercem pressão ~0; a pressão cresce
/// linearmente até 1.0 no instante do deadline (e satura em 1.0 depois).
pub const DEADLINE_HORIZON_SECS: u64 = 7 * 24 * 60 * 60;

const W_URGENCY: f64 = 0.30;
const W_IMPORTANCE: f64 = 0.30;
const W_DEADLINE: f64 = 0.20;
const W_RISK: f64 = 0.10;
const W_COST: f64 = 0.10;

/// Score multidimensional de um nó. Todos os campos contínuos em [0.0, 1.0]
/// (valores fora do intervalo são clampados no cálculo, nunca panic).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeScore {
    /// Quão cedo o resultado é necessário (1.0 = imediato).
    pub urgency: f64,
    /// Impacto no objetivo do plano (1.0 = crítico).
    pub importance: f64,
    /// Prazo absoluto em epoch segundos (None = sem deadline).
    pub deadline_epoch: Option<u64>,
    /// Risco de falha/retrabalho (1.0 = altíssimo). Risco alto SOBE a
    /// prioridade: falhar cedo custa menos que falhar tarde.
    pub risk: f64,
    /// Custo relativo estimado (1.0 = caríssimo). Custo alto DESCE a
    /// prioridade: vitórias baratas primeiro.
    pub cost: f64,
}

impl NodeScore {
    pub fn new(urgency: f64, importance: f64, risk: f64, cost: f64) -> Self {
        Self {
            urgency,
            importance,
            deadline_epoch: None,
            risk,
            cost,
        }
    }

    pub fn with_deadline(mut self, deadline_epoch: u64) -> Self {
        self.deadline_epoch = Some(deadline_epoch);
        self
    }

    /// Pressão de deadline em [0.0, 1.0] relativa a `now_epoch`.
    pub fn deadline_pressure(&self, now_epoch: u64) -> f64 {
        match self.deadline_epoch {
            None => 0.0,
            Some(deadline) => {
                if now_epoch >= deadline {
                    1.0 // deadline estourado: pressão máxima
                } else {
                    let remaining = (deadline - now_epoch) as f64;
                    (1.0 - remaining / DEADLINE_HORIZON_SECS as f64).max(0.0)
                }
            }
        }
    }

    /// Sanitiza um campo: NaN vira o default do campo (clamp não remove NaN
    /// — IEEE 754), garantindo que o composto NUNCA é NaN e a ordenação de
    /// `scored_ready_nodes` permanece total e determinística.
    fn sane(value: f64, default: f64) -> f64 {
        if value.is_nan() {
            default
        } else {
            value.clamp(0.0, 1.0)
        }
    }

    /// Score composto determinístico em [0.0, 1.0]. Garantido não-NaN.
    pub fn composite(&self, now_epoch: u64) -> f64 {
        let d = NodeScore::default();
        let urgency = Self::sane(self.urgency, d.urgency);
        let importance = Self::sane(self.importance, d.importance);
        let risk = Self::sane(self.risk, d.risk);
        let cost = Self::sane(self.cost, d.cost);

        W_URGENCY * urgency
            + W_IMPORTANCE * importance
            + W_DEADLINE * self.deadline_pressure(now_epoch)
            + W_RISK * risk
            + W_COST * (1.0 - cost)
    }
}

impl Default for NodeScore {
    /// Score neutro: nós sem score explícito competem em pé de igualdade.
    fn default() -> Self {
        Self {
            urgency: 0.5,
            importance: 0.5,
            deadline_epoch: None,
            risk: 0.0,
            cost: 0.5,
        }
    }
}

/// Epoch atual em segundos (helper compartilhado pelo executor).
pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_neutro_e_meio_termo() {
        let s = NodeScore::default();
        // 0.3*0.5 + 0.3*0.5 + 0.2*0 + 0.1*0 + 0.1*0.5 = 0.35
        assert!((s.composite(1_000_000) - 0.35).abs() < 1e-9);
    }

    #[test]
    fn urgencia_maxima_supera_neutro() {
        let urgente = NodeScore::new(1.0, 1.0, 0.0, 0.5);
        let neutro = NodeScore::default();
        assert!(urgente.composite(0) > neutro.composite(0));
    }

    #[test]
    fn deadline_estourado_pressao_maxima() {
        let s = NodeScore::default().with_deadline(100);
        assert_eq!(s.deadline_pressure(200), 1.0);
    }

    #[test]
    fn deadline_distante_sem_pressao() {
        let now = 1_000_000;
        let s = NodeScore::default().with_deadline(now + DEADLINE_HORIZON_SECS + 1_000);
        assert_eq!(s.deadline_pressure(now), 0.0);
    }

    #[test]
    fn deadline_proximo_pressao_cresce() {
        let now = 1_000_000;
        // metade do horizonte restante → pressão 0.5
        let s = NodeScore::default().with_deadline(now + DEADLINE_HORIZON_SECS / 2);
        assert!((s.deadline_pressure(now) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn custo_alto_reduz_score() {
        let barato = NodeScore::new(0.5, 0.5, 0.0, 0.0);
        let caro = NodeScore::new(0.5, 0.5, 0.0, 1.0);
        assert!(barato.composite(0) > caro.composite(0));
    }

    #[test]
    fn risco_alto_sobe_prioridade_fail_fast() {
        let arriscado = NodeScore::new(0.5, 0.5, 1.0, 0.5);
        let seguro = NodeScore::new(0.5, 0.5, 0.0, 0.5);
        assert!(arriscado.composite(0) > seguro.composite(0));
    }

    #[test]
    fn valores_fora_do_intervalo_sao_clampados() {
        let s = NodeScore::new(99.0, -5.0, 2.0, -1.0);
        let c = s.composite(0);
        assert!((0.0..=1.0).contains(&c));
    }

    #[test]
    fn nan_em_qualquer_campo_nunca_produz_composto_nan() {
        // NaN escaparia do clamp (IEEE 754) e quebraria a ordenação total.
        for s in [
            NodeScore::new(f64::NAN, 0.5, 0.0, 0.5),
            NodeScore::new(0.5, f64::NAN, 0.0, 0.5),
            NodeScore::new(0.5, 0.5, f64::NAN, 0.5),
            NodeScore::new(0.5, 0.5, 0.0, f64::NAN),
            NodeScore::new(f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        ] {
            let c = s.composite(0);
            assert!(!c.is_nan(), "composite produziu NaN para {:?}", s);
            assert!((0.0..=1.0).contains(&c));
        }
        // Campo NaN cai no default do campo: composto igual ao neutro.
        let all_nan = NodeScore::new(f64::NAN, f64::NAN, f64::NAN, f64::NAN);
        assert!((all_nan.composite(0) - NodeScore::default().composite(0)).abs() < 1e-9);
    }
}
