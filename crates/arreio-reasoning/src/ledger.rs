//! ReasoningLedger — trilha auditável de passos de raciocínio no Blackboard.
//!
//! Cada passo é persistido como tupla na categoria `reasoning` com hash
//! SHA-256 encadeado (estilo audit log do Arreio): adulterar um passo
//! invalida toda a cadeia subsequente. O ledger é stateless entre processos —
//! o estado (seq, último hash) é reconstruído do Blackboard na abertura.

use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Hash gênese da cadeia (64 zeros, como em audit chains).
pub const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Fase do passo dentro do modo de raciocínio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningPhase {
    /// Pensamento do LLM (ReAct) ou cadeia completa (CoT).
    Thought,
    /// Ação proposta pelo LLM e executada pelo harness (ReAct).
    Action,
    /// Observação do resultado da ação, injetada pelo harness (ReAct).
    Observation,
    /// Um ramo de Tree-of-Thoughts.
    Branch,
    /// Seleção determinística de ramo feita pelo harness (ToT).
    Selection,
    /// Programa gerado (Program-Aided).
    Program,
    /// Resposta final.
    Final,
}

impl fmt::Display for ReasoningPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Passo de raciocínio auditável (tupla com hash — PVC-Q2.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningStep {
    pub id: String,
    pub session_id: String,
    /// Sequência monotônica dentro da sessão (0-based).
    pub seq: u32,
    /// PromptMode em vigor (`direct`, `chain_of_thought`, ...).
    pub mode: String,
    pub phase: ReasoningPhase,
    /// Entrada que produziu o passo (prompt, ação, etc.).
    pub input: String,
    /// Saída produzida (resposta do LLM, resultado da ação, etc.).
    pub output: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    /// Epoch em segundos.
    pub timestamp: u64,
    /// Hash do passo anterior (GENESIS_HASH no primeiro).
    pub prev_hash: String,
    /// SHA-256 dos campos canônicos + prev_hash.
    pub hash: String,
}

impl ReasoningStep {
    /// Calcula o hash canônico do passo (excluindo `id` — uuid aleatório —
    /// e o próprio `hash`). Floats formatados com 6 casas para determinismo.
    pub fn compute_hash(&self) -> String {
        let canonical = format!(
            "{}|{}|{}|{:?}|{}|{}|{}|{}|{:.6}|{}|{}",
            self.session_id,
            self.seq,
            self.mode,
            self.phase,
            self.input,
            self.output,
            self.tokens_in,
            self.tokens_out,
            self.cost_usd,
            self.timestamp,
            self.prev_hash
        );
        let digest = Sha256::digest(canonical.as_bytes());
        digest.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Converte para o `ReasoningStep` do monitor meta-cognitivo
    /// (`arreio-memory`), permitindo análise de vieses sobre a trilha auditada.
    pub fn as_meta_cognitive(&self) -> arreio_memory::ReasoningStep {
        arreio_memory::ReasoningStep {
            id: self.id.clone(),
            phase: format!("{:?}", self.phase),
            input: self.input.clone(),
            output: self.output.clone(),
            confidence: 1.0,
            timestamp: self.timestamp,
        }
    }
}

/// Ledger de raciocínio sobre o Blackboard (categoria `reasoning`).
pub struct ReasoningLedger {
    blackboard: Blackboard,
    session_id: String,
    next_seq: u32,
    last_hash: String,
}

impl ReasoningLedger {
    /// Abre o ledger para uma sessão, reconstruindo seq e hash do Blackboard.
    pub fn open(blackboard: Blackboard, session_id: &str) -> Self {
        let existing = Self::load_steps(&blackboard, session_id);
        let (next_seq, last_hash) = match existing.last() {
            Some(last) => (last.seq + 1, last.hash.clone()),
            None => (0, GENESIS_HASH.to_string()),
        };
        Self {
            blackboard,
            session_id: session_id.to_string(),
            next_seq,
            last_hash,
        }
    }

    fn key_prefix(session_id: &str) -> String {
        format!("steps:{}:", session_id)
    }

    fn load_steps(bb: &Blackboard, session_id: &str) -> Vec<ReasoningStep> {
        let mut steps: Vec<ReasoningStep> = bb
            .search_tuples("reasoning", &Self::key_prefix(session_id))
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect();
        steps.sort_by_key(|s| s.seq);
        steps
    }

    fn now_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Registra um passo encadeado e o persiste no Blackboard.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        mode: &str,
        phase: ReasoningPhase,
        input: &str,
        output: &str,
        tokens_in: u64,
        tokens_out: u64,
        cost_usd: f64,
    ) -> Result<ReasoningStep> {
        let mut step = ReasoningStep {
            id: Uuid::new_v4().to_string(),
            session_id: self.session_id.clone(),
            seq: self.next_seq,
            mode: mode.to_string(),
            phase,
            input: input.to_string(),
            output: output.to_string(),
            tokens_in,
            tokens_out,
            cost_usd,
            timestamp: Self::now_epoch(),
            prev_hash: self.last_hash.clone(),
            hash: String::new(),
        };
        step.hash = step.compute_hash();

        let key = format!("{}{:06}", Self::key_prefix(&self.session_id), step.seq);
        self.blackboard
            .put_tuple("reasoning", &key, serde_json::to_value(&step)?)
            .context("gravando passo de raciocínio no Blackboard")?;

        self.next_seq += 1;
        self.last_hash = step.hash.clone();
        Ok(step)
    }

    /// Lê todos os passos da sessão, ordenados por seq.
    pub fn steps(&self) -> Vec<ReasoningStep> {
        Self::load_steps(&self.blackboard, &self.session_id)
    }

    /// Número de passos registrados nesta sessão.
    pub fn len(&self) -> usize {
        self.steps().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Verifica a integridade da cadeia de hashes.
    /// Retorna Ok(true) se íntegra; Ok(false) se houve adulteração ou lacuna.
    pub fn verify_chain(&self) -> Result<bool> {
        let steps = self.steps();
        let mut expected_prev = GENESIS_HASH.to_string();
        for (i, step) in steps.iter().enumerate() {
            if step.seq != i as u32 {
                return Ok(false); // lacuna na sequência
            }
            if step.prev_hash != expected_prev {
                return Ok(false); // elo quebrado
            }
            if step.hash != step.compute_hash() {
                return Ok(false); // conteúdo adulterado
            }
            expected_prev = step.hash.clone();
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn append_encadeia_hashes() {
        let bb = temp_bb();
        let mut ledger = ReasoningLedger::open(bb, "s1");
        let s0 = ledger
            .append("direct", ReasoningPhase::Final, "pergunta", "resposta", 10, 5, 0.001)
            .unwrap();
        let s1 = ledger
            .append("direct", ReasoningPhase::Final, "p2", "r2", 10, 5, 0.001)
            .unwrap();
        assert_eq!(s0.prev_hash, GENESIS_HASH);
        assert_eq!(s1.prev_hash, s0.hash);
        assert_eq!(s0.seq, 0);
        assert_eq!(s1.seq, 1);
    }

    #[test]
    fn verify_chain_integra() {
        let bb = temp_bb();
        let mut ledger = ReasoningLedger::open(bb, "s1");
        for i in 0..5 {
            ledger
                .append(
                    "chain_of_thought",
                    ReasoningPhase::Thought,
                    &format!("in{}", i),
                    &format!("out{}", i),
                    1,
                    1,
                    0.0,
                )
                .unwrap();
        }
        assert!(ledger.verify_chain().unwrap());
        assert_eq!(ledger.len(), 5);
    }

    #[test]
    fn verify_chain_detecta_adulteracao() {
        let bb = temp_bb();
        let mut ledger = ReasoningLedger::open(bb.clone(), "s1");
        ledger
            .append("direct", ReasoningPhase::Final, "in", "out", 1, 1, 0.0)
            .unwrap();
        ledger
            .append("direct", ReasoningPhase::Final, "in2", "out2", 1, 1, 0.0)
            .unwrap();

        // Adultera o output do primeiro passo diretamente no Blackboard.
        let key = "steps:s1:000000";
        let mut raw = bb.get_tuple("reasoning", key).unwrap();
        raw["output"] = serde_json::json!("ADULTERADO");
        bb.put_tuple("reasoning", key, raw).unwrap();

        assert!(!ledger.verify_chain().unwrap());
    }

    #[test]
    fn open_reconstroi_estado_da_sessao() {
        let bb = temp_bb();
        let mut ledger = ReasoningLedger::open(bb.clone(), "s1");
        let s0 = ledger
            .append("direct", ReasoningPhase::Final, "in", "out", 1, 1, 0.0)
            .unwrap();
        drop(ledger);

        // Reabre — deve continuar a cadeia, não recomeçar.
        let mut reopened = ReasoningLedger::open(bb, "s1");
        let s1 = reopened
            .append("direct", ReasoningPhase::Final, "in2", "out2", 1, 1, 0.0)
            .unwrap();
        assert_eq!(s1.seq, 1);
        assert_eq!(s1.prev_hash, s0.hash);
        assert!(reopened.verify_chain().unwrap());
    }

    #[test]
    fn sessoes_sao_isoladas() {
        let bb = temp_bb();
        let mut a = ReasoningLedger::open(bb.clone(), "sessao-a");
        let mut b = ReasoningLedger::open(bb, "sessao-b");
        a.append("direct", ReasoningPhase::Final, "in", "out", 1, 1, 0.0)
            .unwrap();
        b.append("direct", ReasoningPhase::Final, "in", "out", 1, 1, 0.0)
            .unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert!(a.verify_chain().unwrap());
        assert!(b.verify_chain().unwrap());
    }

    #[test]
    fn converte_para_meta_cognitive() {
        let bb = temp_bb();
        let mut ledger = ReasoningLedger::open(bb, "s1");
        let step = ledger
            .append("react_harnessed", ReasoningPhase::Thought, "in", "out", 1, 1, 0.0)
            .unwrap();
        let meta = step.as_meta_cognitive();
        assert_eq!(meta.id, step.id);
        assert_eq!(meta.phase, "Thought");
        assert_eq!(meta.output, "out");
    }
}
