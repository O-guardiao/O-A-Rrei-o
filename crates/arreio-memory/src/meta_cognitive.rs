use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════════
// Tipos do Monitor Meta-Cognitivo
// ═══════════════════════════════════════════════════════════════════════════════

/// Passo de raciocínio registrado para análise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningStep {
    pub id: String,
    pub phase: String,
    pub input: String,
    pub output: String,
    pub confidence: f64,
    pub timestamp: u64,
}

/// Vieses cognitivos detectáveis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CognitiveBias {
    ConfirmationBias,   // busca apenas evidências confirmatórias
    AnchoringBias,      // dependência excessiva da primeira informação
    AvailabilityBias,   // julga pela facilidade de lembrar
    FramingBias,        // influenciado pela formulação do problema
    OverconfidenceBias, // confiança excessiva
}

/// Viés detectado com metadados.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DetectedBias {
    pub bias_type: CognitiveBias,
    pub description: String,
    pub involved_steps: Vec<String>,
    pub severity: f64, // 0.0 - 1.0
}

/// Qualidade agregada do raciocínio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningQuality {
    pub coherence: f64,    // 0.0 - 1.0
    pub completeness: f64, // 0.0 - 1.0
    pub efficiency: f64,   // passos usados / passos mínimos teóricos (invertido)
    pub overall: f64,
}

/// Sugestão de melhoria.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImprovementSuggestion {
    pub category: String,
    pub description: String,
    pub priority: u32, // 1 = baixa, 3 = alta
}

/// Loop de raciocínio detectado.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningLoop {
    pub phase: String,
    pub repeated_output: String,
    pub step_count: usize,
}

/// Tipo de operação para decisão de escalonamento meta-cognitivo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OperationType {
    /// Operações que alteram estado: write, execute, deploy.
    Destructive,
    /// Operações que apenas leem estado.
    ReadOnly,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Monitor Meta-Cognitivo
// ═══════════════════════════════════════════════════════════════════════════════

/// Monitor meta-cognitivo que observa o próprio raciocínio do agente.
pub struct MetaCognitiveMonitor {
    blackboard: Blackboard,
    session_id: String,
}

impl MetaCognitiveMonitor {
    pub fn new(blackboard: Blackboard, session_id: &str) -> Self {
        Self {
            blackboard,
            session_id: session_id.to_string(),
        }
    }

    /// Registra um passo de raciocínio para análise posterior.
    pub fn record_reasoning_step(&self, step: ReasoningStep) -> Result<()> {
        let key = format!("steps:{}", step.id);
        self.blackboard
            .put_tuple(&self.meta_category(), &key, serde_json::to_value(&step)?)
            .context("gravando passo de raciocínio")
    }

    /// Detecta vieses cognitivos no raciocínio armazenado.
    pub fn detect_cognitive_bias(&self) -> Vec<DetectedBias> {
        let steps = self.load_steps();
        let mut biases = Vec::new();

        // ConfirmationBias: mesma output repetida >= 3x
        let mut output_counts: HashMap<String, Vec<String>> = HashMap::new();
        for s in &steps {
            output_counts
                .entry(s.output.clone())
                .or_default()
                .push(s.id.clone());
        }
        for (output, ids) in &output_counts {
            if ids.len() >= 3 {
                biases.push(DetectedBias {
                    bias_type: CognitiveBias::ConfirmationBias,
                    description: format!(
                        "output '{}' repetido {} vezes — possível confirmação seletiva",
                        output,
                        ids.len()
                    ),
                    involved_steps: ids.clone(),
                    severity: (ids.len() as f64 / 10.0).clamp(0.0, 1.0),
                });
            }
        }

        // AnchoringBias: primeiro step domina (>= 70% das outputs contêm substring do primeiro input)
        if let Some(first) = steps.first() {
            let anchored = steps
                .iter()
                .skip(1)
                .filter(|s| {
                    s.output
                        .to_lowercase()
                        .contains(&first.input.to_lowercase())
                })
                .count();
            let ratio = if steps.len() > 1 {
                anchored as f64 / (steps.len() - 1) as f64
            } else {
                0.0
            };
            if ratio >= 0.7 {
                biases.push(DetectedBias {
                    bias_type: CognitiveBias::AnchoringBias,
                    description: format!(
                        "{:.0}% dos passos referenciam o input inicial '{}'",
                        ratio * 100.0,
                        first.input
                    ),
                    involved_steps: steps.iter().map(|s| s.id.clone()).collect(),
                    severity: ratio.clamp(0.0, 1.0),
                });
            }
        }

        // OverconfidenceBias: confiança média > 0.9 com falhas presentes
        if !steps.is_empty() {
            let avg_conf = steps.iter().map(|s| s.confidence).sum::<f64>() / steps.len() as f64;
            let has_failure_like = steps.iter().any(|s| {
                s.output.to_lowercase().contains("fail")
                    || s.output.to_lowercase().contains("erro")
                    || s.output.to_lowercase().contains("error")
            });
            if avg_conf > 0.9 && has_failure_like {
                biases.push(DetectedBias {
                    bias_type: CognitiveBias::OverconfidenceBias,
                    description: format!(
                        "confiança média {:.2} apesar de falhas detectadas",
                        avg_conf
                    ),
                    involved_steps: steps.iter().map(|s| s.id.clone()).collect(),
                    severity: avg_conf.clamp(0.0, 1.0),
                });
            }
        }

        // AvailabilityBias: outputs muito curtos (< 10 chars) em >= 50% dos passos
        if !steps.is_empty() {
            let short = steps.iter().filter(|s| s.output.len() < 10).count();
            let ratio = short as f64 / steps.len() as f64;
            if ratio >= 0.5 {
                biases.push(DetectedBias {
                    bias_type: CognitiveBias::AvailabilityBias,
                    description: format!(
                        "{:.0}% dos outputs são curtos (< 10 chars) — julgamento por disponibilidade",
                        ratio * 100.0
                    ),
                    involved_steps: steps.iter().map(|s| s.id.clone()).collect(),
                    severity: ratio.clamp(0.0, 1.0),
                });
            }
        }

        // FramingBias: alternância rápida de fases (> 3 fases únicas em < 5 passos)
        if steps.len() >= 4 {
            let unique_phases: std::collections::HashSet<_> =
                steps.iter().map(|s| s.phase.clone()).collect();
            if unique_phases.len() > 3 {
                biases.push(DetectedBias {
                    bias_type: CognitiveBias::FramingBias,
                    description: format!(
                        "{} fases distintas em {} passos — sensibilidade ao enquadramento",
                        unique_phases.len(),
                        steps.len()
                    ),
                    involved_steps: steps.iter().map(|s| s.id.clone()).collect(),
                    severity: (unique_phases.len() as f64 / 8.0).clamp(0.0, 1.0),
                });
            }
        }

        biases
    }

    /// Avalia a qualidade do raciocínio.
    pub fn evaluate_reasoning_quality(&self) -> ReasoningQuality {
        let steps = self.load_steps();
        if steps.is_empty() {
            return ReasoningQuality {
                coherence: 0.0,
                completeness: 0.0,
                efficiency: 0.0,
                overall: 0.0,
            };
        }

        // Coerência: similaridade sequencial entre outputs consecutivos (heurística Jaccard simples)
        let mut sim_sum = 0.0;
        let mut sim_count = 0;
        for window in steps.windows(2) {
            let a = &window[0].output.to_lowercase();
            let b = &window[1].output.to_lowercase();
            sim_sum += string_similarity(a, b);
            sim_count += 1;
        }
        let coherence = if sim_count > 0 {
            (sim_sum / sim_count as f64).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // Completude: presença de fases essenciais (observe, orient, decide, act)
        let required = ["observe", "orient", "decide", "act"];
        let present: std::collections::HashSet<_> =
            steps.iter().map(|s| s.phase.to_lowercase()).collect();
        let hits = required.iter().filter(|p| present.contains(**p)).count();
        let completeness = (hits as f64 / required.len() as f64).clamp(0.0, 1.0);

        // Eficiência: ideal teórico = número de fases únicas necessárias
        let ideal = required.len() as f64;
        let actual = steps.len() as f64;
        let efficiency = (ideal / actual).clamp(0.0, 1.0);

        let overall = (coherence * 0.4 + completeness * 0.4 + efficiency * 0.2).clamp(0.0, 1.0);

        ReasoningQuality {
            coherence,
            completeness,
            efficiency,
            overall,
        }
    }

    /// Sugere melhorias com base nos problemas detectados.
    pub fn suggest_improvements(&self) -> Vec<ImprovementSuggestion> {
        let mut suggestions = Vec::new();
        let biases = self.detect_cognitive_bias();
        let quality = self.evaluate_reasoning_quality();

        for bias in &biases {
            match bias.bias_type {
                CognitiveBias::ConfirmationBias => suggestions.push(ImprovementSuggestion {
                    category: "diversidade".to_string(),
                    description: "introduzir passos de contradicação ou hipótese alternativa"
                        .to_string(),
                    priority: 3,
                }),
                CognitiveBias::AnchoringBias => suggestions.push(ImprovementSuggestion {
                    category: "reavaliação".to_string(),
                    description: "revisar premissas iniciais após novas evidências".to_string(),
                    priority: 2,
                }),
                CognitiveBias::OverconfidenceBias => suggestions.push(ImprovementSuggestion {
                    category: "calibração".to_string(),
                    description: "reduzir confiança quando outputs indicam falha".to_string(),
                    priority: 3,
                }),
                CognitiveBias::AvailabilityBias => suggestions.push(ImprovementSuggestion {
                    category: "profundidade".to_string(),
                    description: "expandir outputs com análise detalhada".to_string(),
                    priority: 2,
                }),
                CognitiveBias::FramingBias => suggestions.push(ImprovementSuggestion {
                    category: "estabilidade".to_string(),
                    description: "manter fases consistentes antes de transicionar".to_string(),
                    priority: 2,
                }),
            }
        }

        if quality.coherence < 0.5 {
            suggestions.push(ImprovementSuggestion {
                category: "coerência".to_string(),
                description: "aumentar continuidade lógica entre passos consecutivos".to_string(),
                priority: 3,
            });
        }
        if quality.completeness < 1.0 {
            suggestions.push(ImprovementSuggestion {
                category: "cobertura".to_string(),
                description: "incluir as quatro fases OODA no raciocínio".to_string(),
                priority: 2,
            });
        }
        if quality.efficiency < 0.5 {
            suggestions.push(ImprovementSuggestion {
                category: "economia".to_string(),
                description: "consolidar passos redundantes".to_string(),
                priority: 1,
            });
        }

        // Remove duplicatas exatas
        suggestions.sort_by(|a, b| a.priority.cmp(&b.priority).reverse());
        let mut seen = std::collections::HashSet::new();
        suggestions.retain(|s| seen.insert((s.category.clone(), s.description.clone())));

        suggestions
    }

    /// Verifica se o agente está em loop de raciocínio.
    pub fn detect_reasoning_loop(&self) -> Option<ReasoningLoop> {
        let steps = self.load_steps();
        let mut max_repeat = 0usize;
        let mut loop_phase = String::new();
        let mut loop_output = String::new();

        // Agrupa passos consecutivos com mesma fase + output
        let mut current_count = 0usize;
        let mut current_phase = String::new();
        let mut current_output = String::new();

        for s in &steps {
            if s.phase == current_phase && s.output == current_output {
                current_count += 1;
            } else {
                if current_count > max_repeat {
                    max_repeat = current_count;
                    loop_phase = current_phase.clone();
                    loop_output = current_output.clone();
                }
                current_phase = s.phase.clone();
                current_output = s.output.clone();
                current_count = 1;
            }
        }
        if current_count > max_repeat {
            max_repeat = current_count;
            loop_phase = current_phase;
            loop_output = current_output;
        }

        if max_repeat >= 3 {
            Some(ReasoningLoop {
                phase: loop_phase,
                repeated_output: loop_output,
                step_count: max_repeat,
            })
        } else {
            None
        }
    }

    /// Persiste o estado completo no Blackboard.
    pub fn persist_state(&self) -> Result<()> {
        let steps = self.load_steps();
        let biases = self.detect_cognitive_bias();
        let quality = self.evaluate_reasoning_quality();

        self.blackboard
            .put_tuple(
                &self.meta_category(),
                "state",
                serde_json::json!({
                    "session_id": self.session_id,
                    "step_count": steps.len(),
                    "biases": biases,
                    "quality": quality,
                }),
            )
            .context("persistindo estado meta-cognitivo")
    }

    // ── Internos ──────────────────────────────────────────────────────────────

    fn meta_category(&self) -> String {
        format!("meta:{}", self.session_id)
    }

    fn load_steps(&self) -> Vec<ReasoningStep> {
        let tuples = self
            .blackboard
            .search_tuples(&self.meta_category(), "steps:");
        let mut steps: Vec<ReasoningStep> = tuples
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value::<ReasoningStep>(v).ok())
            .collect();
        steps.sort_by_key(|s| s.timestamp);
        steps
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Explicação TEIRESIAS-style
// ═══════════════════════════════════════════════════════════════════════════════

/// Explorador de cadeia de raciocínio no estilo TEIRESIAS.
/// Navega pelos `ReasoningStep`s armazenados no Blackboard e gera
/// explicações legíveis de COMO e POR QUE uma decisão foi tomada.
pub struct TeiresiasExplainer {
    blackboard: Blackboard,
    session_id: String,
}

impl TeiresiasExplainer {
    pub fn new(blackboard: Blackboard, session_id: &str) -> Self {
        Self {
            blackboard,
            session_id: session_id.to_string(),
        }
    }

    /// Explica COMO uma decisão foi tomada, navegando pela cadeia de passos
    /// que precedem e incluem o `step_id` solicitado.
    pub fn explain_how(&self, step_id: &str) -> String {
        let steps = self.load_steps();
        if steps.is_empty() {
            return format!(
                "Nenhum passo de raciocínio encontrado para sessão '{}'.",
                self.session_id
            );
        }

        let target_idx = steps.iter().position(|s| s.id == step_id);
        let up_to = match target_idx {
            Some(idx) => &steps[..=idx],
            None => &steps[..],
        };

        let mut explanation = format!(
            "Explicação COMO (sessão '{}', passo '{}'):",
            self.session_id, step_id
        );
        for (i, s) in up_to.iter().enumerate() {
            explanation.push_str(&format!(
                "\n  {}. [{} | conf={:.2}] input='{}' → output='{}'",
                i + 1,
                s.phase,
                s.confidence,
                s.input,
                s.output
            ));
        }
        explanation
    }

    /// Explica POR QUE uma decisão foi tomada, analisando o passo específico
    /// e seu contexto imediato (passo anterior, confiança, fase).
    pub fn explain_why(&self, step_id: &str) -> String {
        let steps = self.load_steps();
        let target = match steps.iter().find(|s| s.id == step_id) {
            Some(s) => s.clone(),
            None => {
                return format!(
                    "Passo '{}' não encontrado na sessão '{}'.",
                    step_id, self.session_id
                );
            }
        };

        let prev = steps
            .iter()
            .find(|s| s.timestamp < target.timestamp)
            .cloned();

        let mut explanation = format!(
            "Explicação POR QUE (sessão '{}', passo '{}'):",
            self.session_id, step_id
        );
        explanation.push_str(&format!(
            "\n  Decisão tomada na fase '{}' com confiança {:.2}.",
            target.phase, target.confidence
        ));
        explanation.push_str(&format!("\n  Input considerado: '{}'.", target.input));
        explanation.push_str(&format!("\n  Output produzido: '{}'.", target.output));

        if let Some(p) = prev {
            explanation.push_str(&format!(
                "\n  Contexto anterior (passo '{}'): '{}' → '{}'.",
                p.id, p.input, p.output
            ));
        } else {
            explanation
                .push_str("\n  Este foi o primeiro passo do raciocínio; não há contexto anterior.");
        }

        if target.confidence < 0.5 {
            explanation
                .push_str("\n  Atenção: confiança baixa (< 0.5) — a decisão pode ser frágil.");
        } else if target.confidence > 0.9 {
            explanation.push_str(
                "\n  Confiança alta (> 0.9) — decisão firme com pouca incerteza declarada.",
            );
        }

        explanation
    }

    /// Persiste uma explicação HOW no Blackboard sob `meta:explain:{session_id}:how:{step_id}`.
    pub fn persist_how(&self, step_id: &str) -> Result<()> {
        let text = self.explain_how(step_id);
        let key = format!("how:{}", step_id);
        self.blackboard
            .put_tuple(&self.explain_category(), &key, serde_json::to_value(&text)?)
            .context("persistindo explicação HOW")
    }

    /// Persiste uma explicação WHY no Blackboard sob `meta:explain:{session_id}:why:{step_id}`.
    pub fn persist_why(&self, step_id: &str) -> Result<()> {
        let text = self.explain_why(step_id);
        let key = format!("why:{}", step_id);
        self.blackboard
            .put_tuple(&self.explain_category(), &key, serde_json::to_value(&text)?)
            .context("persistindo explicação WHY")
    }

    fn explain_category(&self) -> String {
        format!("meta:explain:{}", self.session_id)
    }

    fn load_steps(&self) -> Vec<ReasoningStep> {
        let tuples = self
            .blackboard
            .search_tuples(&format!("meta:{}", self.session_id), "steps:");
        let mut steps: Vec<ReasoningStep> = tuples
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value::<ReasoningStep>(v).ok())
            .collect();
        steps.sort_by_key(|s| s.timestamp);
        steps
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Entropia Semântica para Detecção de Alucinações
// ═══════════════════════════════════════════════════════════════════════════════

/// Calcula entropia semântica entre múltiplas amostras de resposta.
/// Entropia alta indica inconsistência semântica — provável alucinação.
pub struct SemanticEntropy;

impl SemanticEntropy {
    /// Calcula a entropia normalizada [0.0, 1.0] a partir de múltiplas amostras.
    /// Usa clustering por similaridade Jaccard como proxy semântico.
    pub fn compute(samples: Vec<String>) -> f64 {
        if samples.len() < 2 {
            return 0.0;
        }

        // Clusteriza amostras similares usando threshold de Jaccard
        let threshold = 0.6;
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        let mut assigned = vec![false; samples.len()];

        for i in 0..samples.len() {
            if assigned[i] {
                continue;
            }
            let mut cluster = vec![i];
            assigned[i] = true;
            for j in (i + 1)..samples.len() {
                if !assigned[j] && jaccard_similarity(&samples[i], &samples[j]) >= threshold {
                    cluster.push(j);
                    assigned[j] = true;
                }
            }
            clusters.push(cluster);
        }

        // Entropia de Shannon normalizada
        let n = samples.len() as f64;
        let mut entropy = 0.0;
        for c in &clusters {
            let p = c.len() as f64 / n;
            if p > 0.0 {
                entropy -= p * p.log2();
            }
        }
        let max_entropy = (samples.len() as f64).log2();
        if max_entropy > 0.0 {
            (entropy / max_entropy).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Determina se a entropia indica alucinação dado um threshold.
    pub fn is_hallucination(entropy: f64, threshold: f64) -> bool {
        entropy > threshold
    }

    /// Persiste o valor de entropia no Blackboard.
    pub fn persist(
        blackboard: &Blackboard,
        session_id: &str,
        entropy: f64,
        threshold: f64,
        is_hallucination: bool,
    ) -> Result<()> {
        let category = format!("meta:entropy:{}", session_id);
        blackboard
            .put_tuple(
                &category,
                "value",
                serde_json::json!({
                    "entropy": entropy,
                    "threshold": threshold,
                    "is_hallucination": is_hallucination,
                }),
            )
            .context("persistindo entropia semântica")
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Modelo de Custo para Escalonamento Meta-Cognitivo
// ═══════════════════════════════════════════════════════════════════════════════

/// Modelo de custo que decide se uma operação deve ser escalonada
/// para revisão meta-cognitiva.
pub struct MetaCostModel;

impl MetaCostModel {
    /// Decide se deve escalonar com base na fórmula:
    /// `cost_meta < prob_error * cost_error`.
    /// Para operações destrutivas (`Destructive`), sempre retorna `true`.
    /// Para operações read-only (`ReadOnly`), aplica a fórmula.
    pub fn should_escalate(
        prob_error: f64,
        cost_error: f64,
        cost_meta: f64,
        op_type: OperationType,
    ) -> bool {
        match op_type {
            OperationType::Destructive => true,
            OperationType::ReadOnly => cost_meta < prob_error * cost_error,
        }
    }

    /// Persiste a decisão de escalonamento no Blackboard.
    pub fn persist_decision(
        blackboard: &Blackboard,
        session_id: &str,
        prob_error: f64,
        cost_error: f64,
        cost_meta: f64,
        op_type: OperationType,
        should_escalate: bool,
    ) -> Result<()> {
        let category = format!("meta:escalate:{}", session_id);
        blackboard
            .put_tuple(
                &category,
                "decision",
                serde_json::json!({
                    "prob_error": prob_error,
                    "cost_error": cost_error,
                    "cost_meta": cost_meta,
                    "op_type": op_type,
                    "should_escalate": should_escalate,
                }),
            )
            .context("persistindo decisão de escalonamento")
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utilitários
// ═══════════════════════════════════════════════════════════════════════════════

/// Similaridade simples baseada em palavras comuns (Jaccard aproximado).
fn string_similarity(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<_> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<_> = b.split_whitespace().collect();
    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    let intersection = words_a.intersection(&words_b).count() as f64;
    let union = words_a.union(&words_b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        (intersection / union).clamp(0.0, 1.0)
    }
}

/// Similaridade Jaccard entre duas strings (tokenizada por palavras).
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    string_similarity(a, b)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_board() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    fn make_step(
        id: &str,
        phase: &str,
        input: &str,
        output: &str,
        confidence: f64,
    ) -> ReasoningStep {
        ReasoningStep {
            id: id.to_string(),
            phase: phase.to_string(),
            input: input.to_string(),
            output: output.to_string(),
            confidence,
            timestamp: id.parse().unwrap_or(0),
        }
    }

    #[test]
    fn records_reasoning_step() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb.clone(), "sess1");
        let step = make_step("1", "observe", "input", "output", 0.8);
        monitor.record_reasoning_step(step.clone()).unwrap();

        let loaded = bb
            .get_tuple("meta:sess1", "steps:1")
            .and_then(|v| serde_json::from_value::<ReasoningStep>(v).ok())
            .unwrap();
        assert_eq!(loaded, step);
    }

    #[test]
    fn detects_confirmation_bias() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess2");
        for i in 0..4 {
            monitor
                .record_reasoning_step(make_step(&i.to_string(), "decide", "q", "always yes", 0.9))
                .unwrap();
        }
        let biases = monitor.detect_cognitive_bias();
        assert!(biases
            .iter()
            .any(|b| b.bias_type == CognitiveBias::ConfirmationBias));
    }

    #[test]
    fn detects_anchoring_bias() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess3");
        monitor
            .record_reasoning_step(make_step("1", "observe", "anchor_value", "initial", 0.8))
            .unwrap();
        for i in 2..6 {
            monitor
                .record_reasoning_step(make_step(
                    &i.to_string(),
                    "decide",
                    &format!("q{}", i),
                    "anchor_value conclusion",
                    0.8,
                ))
                .unwrap();
        }
        let biases = monitor.detect_cognitive_bias();
        assert!(biases
            .iter()
            .any(|b| b.bias_type == CognitiveBias::AnchoringBias));
    }

    #[test]
    fn evaluates_reasoning_quality() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess4");
        monitor
            .record_reasoning_step(make_step("1", "observe", "x", "saw x", 0.7))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("2", "orient", "x", "x is valid", 0.7))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("3", "decide", "x", "proceed with x", 0.7))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("4", "act", "x", "done", 0.7))
            .unwrap();

        let q = monitor.evaluate_reasoning_quality();
        assert!(q.completeness > 0.9);
        assert!(q.coherence > 0.0);
        assert!(q.overall > 0.0);
    }

    #[test]
    fn suggests_improvements() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess5");
        // Cria cenário com ConfirmationBias + baixa completude
        for i in 0..3 {
            monitor
                .record_reasoning_step(make_step(&i.to_string(), "decide", "q", "same", 0.95))
                .unwrap();
        }
        let suggestions = monitor.suggest_improvements();
        assert!(!suggestions.is_empty());
        // Deve conter sugestão de diversidade (confirmation) e cobertura (completude)
        assert!(suggestions.iter().any(|s| s.category == "diversidade"));
        assert!(suggestions.iter().any(|s| s.category == "cobertura"));
    }

    #[test]
    fn detects_reasoning_loop() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess6");
        monitor
            .record_reasoning_step(make_step("1", "observe", "a", "loop", 0.8))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("2", "decide", "b", "loop", 0.8))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("3", "decide", "c", "loop", 0.8))
            .unwrap();

        // Ainda não é loop (não consecutivos com mesma fase+output)
        assert!(monitor.detect_reasoning_loop().is_none());

        monitor
            .record_reasoning_step(make_step("4", "decide", "d", "loop", 0.8))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("5", "decide", "e", "loop", 0.8))
            .unwrap();

        let detected = monitor.detect_reasoning_loop().unwrap();
        assert_eq!(detected.phase, "decide");
        assert_eq!(detected.repeated_output, "loop");
        assert!(detected.step_count >= 3);
    }

    #[test]
    fn persist_state_to_blackboard() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb.clone(), "sess7");
        monitor
            .record_reasoning_step(make_step("1", "observe", "a", "b", 0.8))
            .unwrap();
        monitor.persist_state().unwrap();

        let state = bb
            .get_tuple("meta:sess7", "state")
            .expect("state deve existir");
        assert_eq!(state["session_id"], "sess7");
        assert_eq!(state["step_count"], 1);
    }

    #[test]
    fn multiple_steps_quality_degrades() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess8");
        // Muitos passos redundantes degradam eficiência
        for i in 0..20 {
            let phase = if i % 2 == 0 { "observe" } else { "act" };
            monitor
                .record_reasoning_step(make_step(
                    &i.to_string(),
                    phase,
                    "in",
                    &format!("out{}", i),
                    0.5,
                ))
                .unwrap();
        }
        let q = monitor.evaluate_reasoning_quality();
        assert!(q.efficiency < 0.5); // ideal=4, actual=20
    }

    #[test]
    fn bias_severity_calculation() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess9");
        for i in 0..5 {
            monitor
                .record_reasoning_step(make_step(&i.to_string(), "decide", "q", "same", 0.9))
                .unwrap();
        }
        let biases = monitor.detect_cognitive_bias();
        let confirmation = biases
            .iter()
            .find(|b| b.bias_type == CognitiveBias::ConfirmationBias)
            .unwrap();
        assert!(confirmation.severity > 0.0 && confirmation.severity <= 1.0);
    }

    #[test]
    fn reasoning_loop_with_repeated_steps() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess10");
        for i in 0..4 {
            monitor
                .record_reasoning_step(make_step(&i.to_string(), "orient", "x", "stuck", 0.6))
                .unwrap();
        }
        let detected = monitor.detect_reasoning_loop().unwrap();
        assert_eq!(detected.phase, "orient");
        assert_eq!(detected.repeated_output, "stuck");
        assert_eq!(detected.step_count, 4);
    }

    #[test]
    fn overconfidence_bias_detected() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess11");
        monitor
            .record_reasoning_step(make_step("1", "observe", "x", "error found", 0.95))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("2", "decide", "x", "still ok", 0.98))
            .unwrap();
        let biases = monitor.detect_cognitive_bias();
        assert!(biases
            .iter()
            .any(|b| b.bias_type == CognitiveBias::OverconfidenceBias));
    }

    #[test]
    fn availability_bias_detected() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb, "sess12");
        for i in 0..4 {
            monitor
                .record_reasoning_step(make_step(&i.to_string(), "decide", "x", "ok", 0.8))
                .unwrap();
        }
        let biases = monitor.detect_cognitive_bias();
        assert!(biases
            .iter()
            .any(|b| b.bias_type == CognitiveBias::AvailabilityBias));
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Testes TEIRESIAS
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn teiresias_explain_how_traverses_chain() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb.clone(), "sess_t1");
        monitor
            .record_reasoning_step(make_step("1", "observe", "req", "saw req", 0.8))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("2", "orient", "req", "req is valid", 0.85))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("3", "decide", "req", "proceed", 0.9))
            .unwrap();

        let explainer = TeiresiasExplainer::new(bb, "sess_t1");
        let how = explainer.explain_how("3");
        assert!(how.contains("Explicação COMO"));
        assert!(how.contains("observe"));
        assert!(how.contains("orient"));
        assert!(how.contains("decide"));
        assert!(how.contains("proceed"));
    }

    #[test]
    fn teiresias_explain_why_justifies_decision() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb.clone(), "sess_t2");
        monitor
            .record_reasoning_step(make_step("1", "observe", "bug", "found bug", 0.8))
            .unwrap();
        monitor
            .record_reasoning_step(make_step("2", "decide", "bug", "fix it", 0.75))
            .unwrap();

        let explainer = TeiresiasExplainer::new(bb, "sess_t2");
        let why = explainer.explain_why("2");
        assert!(why.contains("Explicação POR QUE"));
        assert!(why.contains("decide"));
        assert!(why.contains("fix it"));
        assert!(why.contains("Contexto anterior"));
    }

    #[test]
    fn teiresias_persist_how_and_why() {
        let bb = temp_board();
        let monitor = MetaCognitiveMonitor::new(bb.clone(), "sess_t3");
        monitor
            .record_reasoning_step(make_step("1", "act", "x", "done", 0.9))
            .unwrap();

        let explainer = TeiresiasExplainer::new(bb.clone(), "sess_t3");
        explainer.persist_how("1").unwrap();
        explainer.persist_why("1").unwrap();

        let how_val = bb.get_tuple("meta:explain:sess_t3", "how:1").unwrap();
        let why_val = bb.get_tuple("meta:explain:sess_t3", "why:1").unwrap();
        assert!(how_val.as_str().unwrap().contains("Explicação COMO"));
        assert!(why_val.as_str().unwrap().contains("Explicação POR QUE"));
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Testes Entropia Semântica
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn semantic_entropy_low_for_consistent_samples() {
        let samples = vec![
            "the quick brown fox jumps over the lazy dog".to_string(),
            "the quick brown fox jumps over the lazy dog".to_string(),
            "the quick brown fox jumps over the lazy dog".to_string(),
        ];
        let entropy = SemanticEntropy::compute(samples);
        assert!(
            entropy < 0.3,
            "entropia deveria ser baixa para amostras consistentes, got {}",
            entropy
        );
        assert!(!SemanticEntropy::is_hallucination(entropy, 0.5));
    }

    #[test]
    fn semantic_entropy_high_for_inconsistent_samples() {
        let samples = vec![
            "the quick brown fox jumps over the lazy dog".to_string(),
            "completely different sentence with no overlap".to_string(),
            "yet another unrelated string of words here".to_string(),
            "totally disjoint meaning from all others".to_string(),
        ];
        let entropy = SemanticEntropy::compute(samples);
        assert!(
            entropy > 0.5,
            "entropia deveria ser alta para amostras inconsistentes, got {}",
            entropy
        );
        assert!(SemanticEntropy::is_hallucination(entropy, 0.5));
    }

    #[test]
    fn semantic_entropy_persist_to_blackboard() {
        let bb = temp_board();
        SemanticEntropy::persist(&bb, "sess_e1", 0.85, 0.5, true).unwrap();
        let val = bb.get_tuple("meta:entropy:sess_e1", "value").unwrap();
        assert_eq!(val["entropy"], 0.85);
        assert_eq!(val["threshold"], 0.5);
        assert_eq!(val["is_hallucination"], true);
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Testes MetaCostModel
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn metacost_escalates_when_error_cost_high() {
        // prob_error=0.5, cost_error=100, cost_meta=10
        // 10 < 0.5 * 100 = 50 → true
        assert!(MetaCostModel::should_escalate(
            0.5,
            100.0,
            10.0,
            OperationType::ReadOnly
        ));
    }

    #[test]
    fn metacost_does_not_escalate_when_meta_too_expensive() {
        // prob_error=0.1, cost_error=50, cost_meta=10
        // 10 < 0.1 * 50 = 5 → false
        assert!(!MetaCostModel::should_escalate(
            0.1,
            50.0,
            10.0,
            OperationType::ReadOnly
        ));
    }

    #[test]
    fn metacost_destructive_always_escalates() {
        // Mesmo com custo meta alto e erro barato
        assert!(MetaCostModel::should_escalate(
            0.01,
            1.0,
            1000.0,
            OperationType::Destructive
        ));
        assert!(MetaCostModel::should_escalate(
            0.0,
            0.0,
            0.0,
            OperationType::Destructive
        ));
    }

    #[test]
    fn metacost_readonly_can_be_bypassed() {
        // prob_error=0.1, cost_error=50, cost_meta=10
        // 10 < 5 → false, então read-only pode ser bypassada
        assert!(!MetaCostModel::should_escalate(
            0.1,
            50.0,
            10.0,
            OperationType::ReadOnly
        ));
    }

    #[test]
    fn metacost_persist_decision_to_blackboard() {
        let bb = temp_board();
        MetaCostModel::persist_decision(
            &bb,
            "sess_c1",
            0.3,
            200.0,
            10.0,
            OperationType::Destructive,
            true,
        )
        .unwrap();
        let val = bb.get_tuple("meta:escalate:sess_c1", "decision").unwrap();
        assert_eq!(val["prob_error"], 0.3);
        assert_eq!(val["cost_error"], 200.0);
        assert_eq!(val["should_escalate"], true);
    }
}
