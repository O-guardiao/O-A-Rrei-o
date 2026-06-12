//! Evaluation estruturada — EvalSet/EvalCase + detecção de regressão (PVC-Q2.2).
//!
//! Avaliação determinística e auditável: cada `EvalCase` tem uma expectativa
//! verificável (Contains/Equals/Regex), o runner produz um `EvalReport`
//! ponderado e o `RegressionDetector` compara contra baseline com threshold
//! padrão de 5% (regra do plano Q2: regression detection > 5%).
//!
//! Relatórios são persistidos no Blackboard (categoria `eval`) para que o
//! histórico sobreviva entre sessões e o baseline seja explícito.

use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Threshold padrão de regressão: queda de score > 5% bloqueia.
pub const DEFAULT_REGRESSION_THRESHOLD: f64 = 0.05;

// ── Casos e conjuntos ─────────────────────────────────────────────────────────

/// Expectativa verificável de um caso de avaliação.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expectation {
    /// A saída deve conter a substring.
    Contains(String),
    /// A saída (trim) deve ser exatamente igual.
    Equals(String),
    /// A saída deve casar com a regex.
    Regex(String),
}

impl Expectation {
    /// Avalia a saída do candidato. Retorna (passou, detalhe).
    pub fn evaluate(&self, actual: &str) -> (bool, String) {
        match self {
            Expectation::Contains(s) => {
                let ok = actual.contains(s.as_str());
                (ok, format!("contains('{}') = {}", s, ok))
            }
            Expectation::Equals(s) => {
                let ok = actual.trim() == s.trim();
                (ok, format!("equals('{}') = {}", s, ok))
            }
            Expectation::Regex(pattern) => match Regex::new(pattern) {
                Ok(re) => {
                    let ok = re.is_match(actual);
                    (ok, format!("regex('{}') = {}", pattern, ok))
                }
                Err(e) => (false, format!("regex inválida '{}': {}", pattern, e)),
            },
        }
    }
}

/// Caso individual de avaliação.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    pub id: String,
    /// Entrada fornecida ao candidato (prompt, spec, etc.).
    pub input: String,
    pub expected: Expectation,
    /// Peso no score agregado (padrão 1.0).
    pub weight: f64,
    pub tags: Vec<String>,
}

impl EvalCase {
    pub fn new(id: impl Into<String>, input: impl Into<String>, expected: Expectation) -> Self {
        Self {
            id: id.into(),
            input: input.into(),
            expected,
            weight: 1.0,
            tags: Vec::new(),
        }
    }

    pub fn with_weight(mut self, weight: f64) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// Conjunto estruturado de casos de avaliação.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSet {
    pub id: String,
    pub name: String,
    pub description: String,
    pub cases: Vec<EvalCase>,
}

impl EvalSet {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            cases: Vec::new(),
        }
    }

    pub fn add_case(mut self, case: EvalCase) -> Self {
        self.cases.push(case);
        self
    }
}

// ── Resultados ────────────────────────────────────────────────────────────────

/// Resultado de um caso.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseOutcome {
    pub case_id: String,
    pub passed: bool,
    /// Score do caso (0.0 ou 1.0 para expectativas determinísticas).
    pub score: f64,
    /// Saída real do candidato (truncada a 2000 chars para o relatório).
    pub actual: String,
    pub detail: String,
}

/// Relatório agregado de uma execução de EvalSet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub set_id: String,
    pub timestamp: u64,
    pub total_cases: usize,
    pub passed_cases: usize,
    /// Score ponderado pelos pesos dos casos, em [0.0, 1.0].
    pub weighted_score: f64,
    pub outcomes: Vec<CaseOutcome>,
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Executa um EvalSet contra um candidato (função síncrona, stateless).
pub struct EvalRunner;

impl EvalRunner {
    /// Roda todos os casos. Erros do candidato viram caso reprovado
    /// (nunca abortam a suite — evidência completa é obrigatória).
    pub fn run<F>(set: &EvalSet, candidate: F) -> EvalReport
    where
        F: Fn(&EvalCase) -> Result<String>,
    {
        let mut outcomes = Vec::with_capacity(set.cases.len());
        let mut weighted_sum = 0.0;
        let mut weight_total = 0.0;
        let mut passed_cases = 0;

        for case in &set.cases {
            let outcome = match candidate(case) {
                Ok(actual) => {
                    let (passed, detail) = case.expected.evaluate(&actual);
                    CaseOutcome {
                        case_id: case.id.clone(),
                        passed,
                        score: if passed { 1.0 } else { 0.0 },
                        actual: actual.chars().take(2000).collect(),
                        detail,
                    }
                }
                Err(e) => CaseOutcome {
                    case_id: case.id.clone(),
                    passed: false,
                    score: 0.0,
                    actual: String::new(),
                    detail: format!("candidato falhou: {}", e),
                },
            };
            if outcome.passed {
                passed_cases += 1;
            }
            weighted_sum += outcome.score * case.weight;
            weight_total += case.weight;
            outcomes.push(outcome);
        }

        let weighted_score = if weight_total > 0.0 {
            weighted_sum / weight_total
        } else {
            0.0
        };

        EvalReport {
            set_id: set.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            total_cases: set.cases.len(),
            passed_cases,
            weighted_score,
            outcomes,
        }
    }
}

// ── Detecção de regressão ─────────────────────────────────────────────────────

/// Veredito da comparação baseline × atual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionVerdict {
    /// True se a queda de score ultrapassou o threshold.
    pub regression_detected: bool,
    pub baseline_score: f64,
    pub current_score: f64,
    /// current - baseline (negativo = piora).
    pub delta: f64,
    pub threshold: f64,
    /// Casos que passavam no baseline e falharam agora.
    pub regressed_cases: Vec<String>,
    /// Casos que falhavam no baseline e passaram agora.
    pub improved_cases: Vec<String>,
}

/// Compara relatórios e detecta regressão acima do threshold.
pub struct RegressionDetector {
    threshold: f64,
}

impl RegressionDetector {
    /// Threshold padrão: 5% (plano Q2).
    pub fn new() -> Self {
        Self {
            threshold: DEFAULT_REGRESSION_THRESHOLD,
        }
    }

    pub fn with_threshold(threshold: f64) -> Self {
        Self { threshold }
    }

    pub fn compare(&self, baseline: &EvalReport, current: &EvalReport) -> RegressionVerdict {
        let delta = current.weighted_score - baseline.weighted_score;

        let mut regressed = Vec::new();
        let mut improved = Vec::new();
        for cur in &current.outcomes {
            if let Some(base) = baseline.outcomes.iter().find(|b| b.case_id == cur.case_id) {
                if base.passed && !cur.passed {
                    regressed.push(cur.case_id.clone());
                } else if !base.passed && cur.passed {
                    improved.push(cur.case_id.clone());
                }
            }
        }

        RegressionVerdict {
            regression_detected: delta < -self.threshold,
            baseline_score: baseline.weighted_score,
            current_score: current.weighted_score,
            delta,
            threshold: self.threshold,
            regressed_cases: regressed,
            improved_cases: improved,
        }
    }
}

impl Default for RegressionDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ── Persistência no Blackboard ────────────────────────────────────────────────

/// Armazena relatórios e baseline no Blackboard (categoria `eval`).
pub struct EvalStore {
    blackboard: Blackboard,
}

impl EvalStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Persiste um relatório com chave `report:{set_id}:{timestamp}:{uuid}`.
    /// O sufixo único evita colisão entre execuções no mesmo segundo.
    pub fn save_report(&self, report: &EvalReport) -> Result<()> {
        let key = format!(
            "report:{}:{:020}:{}",
            report.set_id,
            report.timestamp,
            uuid::Uuid::new_v4()
        );
        self.blackboard
            .put_tuple("eval", &key, serde_json::to_value(report)?)
            .context("persistindo EvalReport")
    }

    /// Define o relatório como baseline do conjunto.
    pub fn set_baseline(&self, report: &EvalReport) -> Result<()> {
        let key = format!("baseline:{}", report.set_id);
        self.blackboard
            .put_tuple("eval", &key, serde_json::to_value(report)?)
            .context("persistindo baseline")
    }

    /// Carrega o baseline do conjunto, se existir.
    pub fn baseline(&self, set_id: &str) -> Option<EvalReport> {
        self.blackboard
            .get_tuple("eval", &format!("baseline:{}", set_id))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Lista todos os relatórios persistidos de um conjunto (ordem cronológica).
    pub fn reports(&self, set_id: &str) -> Vec<EvalReport> {
        let mut reports: Vec<EvalReport> = self
            .blackboard
            .search_tuples("eval", &format!("report:{}:", set_id))
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect();
        reports.sort_by_key(|r| r.timestamp);
        reports
    }

    /// Fluxo completo: salva o relatório, compara com o baseline (se houver)
    /// e promove o relatório a baseline quando não há regressão.
    pub fn record_and_check(
        &self,
        report: &EvalReport,
        detector: &RegressionDetector,
    ) -> Result<Option<RegressionVerdict>> {
        self.save_report(report)?;
        match self.baseline(&report.set_id) {
            Some(baseline) => {
                let verdict = detector.compare(&baseline, report);
                if !verdict.regression_detected {
                    self.set_baseline(report)?;
                }
                Ok(Some(verdict))
            }
            None => {
                // Primeiro relatório vira baseline automaticamente.
                self.set_baseline(report)?;
                Ok(None)
            }
        }
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

    fn sample_set() -> EvalSet {
        EvalSet::new("set1", "Suite de exemplo", "testes determinísticos")
            .add_case(EvalCase::new(
                "c1",
                "2+2",
                Expectation::Equals("4".into()),
            ))
            .add_case(EvalCase::new(
                "c2",
                "capital do Brasil",
                Expectation::Contains("Brasília".into()),
            ))
            .add_case(EvalCase::new(
                "c3",
                "gerar versão semver",
                Expectation::Regex(r"^\d+\.\d+\.\d+$".into()),
            ))
    }

    fn perfect_candidate(case: &EvalCase) -> Result<String> {
        Ok(match case.id.as_str() {
            "c1" => "4".to_string(),
            "c2" => "a capital é Brasília".to_string(),
            _ => "1.2.3".to_string(),
        })
    }

    #[test]
    fn runner_score_perfeito() {
        let report = EvalRunner::run(&sample_set(), perfect_candidate);
        assert_eq!(report.total_cases, 3);
        assert_eq!(report.passed_cases, 3);
        assert!((report.weighted_score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn runner_erro_do_candidato_reprova_sem_abortar() {
        let report = EvalRunner::run(&sample_set(), |case| {
            if case.id == "c2" {
                anyhow::bail!("provider indisponível")
            }
            perfect_candidate(case)
        });
        assert_eq!(report.total_cases, 3);
        assert_eq!(report.passed_cases, 2);
        let failed = report.outcomes.iter().find(|o| o.case_id == "c2").unwrap();
        assert!(failed.detail.contains("candidato falhou"));
    }

    #[test]
    fn pesos_afetam_score() {
        let set = EvalSet::new("w", "pesos", "")
            .add_case(
                EvalCase::new("pesado", "x", Expectation::Equals("ok".into())).with_weight(3.0),
            )
            .add_case(EvalCase::new("leve", "y", Expectation::Equals("ok".into())));
        // Apenas o caso pesado passa → score = 3/4.
        let report = EvalRunner::run(&set, |case| {
            Ok(if case.id == "pesado" { "ok" } else { "errado" }.to_string())
        });
        assert!((report.weighted_score - 0.75).abs() < 1e-9);
    }

    #[test]
    fn regressao_acima_de_5_por_cento_detectada() {
        let baseline = EvalRunner::run(&sample_set(), perfect_candidate);
        // Candidato degradado: falha em c2 (1/3 dos casos → queda ~33%).
        let current = EvalRunner::run(&sample_set(), |case| {
            if case.id == "c2" {
                Ok("não sei".to_string())
            } else {
                perfect_candidate(case)
            }
        });
        let verdict = RegressionDetector::new().compare(&baseline, &current);
        assert!(verdict.regression_detected);
        assert_eq!(verdict.regressed_cases, vec!["c2".to_string()]);
        assert!(verdict.delta < -0.05);
    }

    #[test]
    fn queda_pequena_nao_e_regressao() {
        let baseline = EvalRunner::run(&sample_set(), perfect_candidate);
        let mut current = baseline.clone();
        current.weighted_score = baseline.weighted_score - 0.04; // queda de 4%
        let verdict = RegressionDetector::new().compare(&baseline, &current);
        assert!(!verdict.regression_detected);
    }

    #[test]
    fn melhora_lista_improved_cases() {
        let degraded = EvalRunner::run(&sample_set(), |case| {
            if case.id == "c1" {
                Ok("5".to_string())
            } else {
                perfect_candidate(case)
            }
        });
        let current = EvalRunner::run(&sample_set(), perfect_candidate);
        let verdict = RegressionDetector::new().compare(&degraded, &current);
        assert!(!verdict.regression_detected);
        assert_eq!(verdict.improved_cases, vec!["c1".to_string()]);
    }

    #[test]
    fn store_roundtrip_e_baseline_automatico() {
        let bb = temp_bb();
        let store = EvalStore::new(bb);
        let report = EvalRunner::run(&sample_set(), perfect_candidate);

        // Primeiro registro: vira baseline, sem veredito.
        let verdict = store
            .record_and_check(&report, &RegressionDetector::new())
            .unwrap();
        assert!(verdict.is_none());
        assert!(store.baseline("set1").is_some());

        // Segundo registro degradado: regressão detectada, baseline preservado.
        let degraded = EvalRunner::run(&sample_set(), |_| Ok("errado".to_string()));
        let verdict = store
            .record_and_check(&degraded, &RegressionDetector::new())
            .unwrap()
            .unwrap();
        assert!(verdict.regression_detected);
        let baseline = store.baseline("set1").unwrap();
        assert!((baseline.weighted_score - 1.0).abs() < f64::EPSILON);
        assert_eq!(store.reports("set1").len(), 2);
    }

    #[test]
    fn regex_invalida_reprova_caso() {
        let set = EvalSet::new("rx", "regex", "")
            .add_case(EvalCase::new("c", "x", Expectation::Regex("([".into())));
        let report = EvalRunner::run(&set, |_| Ok("qualquer".to_string()));
        assert_eq!(report.passed_cases, 0);
        assert!(report.outcomes[0].detail.contains("regex inválida"));
    }
}
