//! IG&C — Implicit Guidance and Control (5 camadas de regulação).
//!
//! Implementa o subsistema de homeostase do OODA-C, inspirado em Ashby (1948)
//! e na Homeostase Artificial. Cada camada é um filtro que autoriza ou bloqueia
//! a passagem de uma ação para a fase Act sem deliberação completa.
//!
//! # Camadas
//!
//! 1. **SYS-IDENTITY** — Valida alinhamento com missão do sistema.
//! 2. **BOUNDARY** — Verifica se a ação respeita limites de recursos.
//! 3. **NORMS** — Valida conformidade com regras operacionais.
//! 4. **FEEDBACK** — Ajusta thresholds baseado em outcomes históricos.
//! 5. **SETPOINT** — Mantém variáveis essenciais dentro de faixas seguras.

use std::collections::HashMap;

/// Limiar de confiança para bypass da fase Decide (IG&C).
pub const THETA_IGC: f64 = 0.85;

/// Limiares por camada de regulação.
pub const THETA_IDENTITY: f64 = 0.70;
pub const THETA_BOUNDARY: f64 = 0.75;
pub const THETA_NORMS: f64 = 0.80;
pub const THETA_FEEDBACK: f64 = 0.85;
pub const THETA_SETPOINT: f64 = 0.90;

/// Modelo de orientação usado na fase Orient do OODA-C.
/// Contém o operador implícito que pode ativar o bypass de Decide.
#[derive(Debug, Clone, PartialEq)]
pub struct OrientationModel {
    pub confidence: f64,
    pub task_type: String,
    pub implicit_operator: String,
    pub strategy: String,
}

impl OrientationModel {
    /// Retorna `true` se a confiança for suficiente para pular a deliberação
    /// e ir direto para Act (Implicit Guidance and Control).
    pub fn should_bypass_decide(&self) -> bool {
        self.confidence >= THETA_IGC
    }
}

// ── Camada 1: System Identity ────────────────────────────────────────────────
//
// Valida que a ação está alinhada com a missão do sistema. Ex: um agente
// de coding não deve fazer trading de criptomoedas.

/// Validacão de identidade do sistema.
#[derive(Debug, Clone)]
pub struct IdentityCheck {
    /// Domínios permitidos (ex: ["code", "docs", "debug"]).
    pub allowed_domains: Vec<String>,
    /// Score mínimo de alinhamento (0.0-1.0).
    pub min_alignment: f64,
}

impl IdentityCheck {
    pub fn new(allowed_domains: Vec<String>) -> Self {
        Self {
            allowed_domains,
            min_alignment: THETA_IDENTITY,
        }
    }

    /// Verifica se a tarefa está em um domínio permitido.
    pub fn check(&self, task_type: &str) -> bool {
        self.allowed_domains
            .iter()
            .any(|d| task_type.to_lowercase().contains(&d.to_lowercase()))
    }

    /// Calcula score de alinhamento (simplificado: match de domínio).
    pub fn alignment_score(&self, task_type: &str) -> f64 {
        if self.allowed_domains.is_empty() {
            return 1.0;
        }
        let matched = self
            .allowed_domains
            .iter()
            .filter(|d| task_type.to_lowercase().contains(&d.to_lowercase()))
            .count();
        matched as f64 / self.allowed_domains.len() as f64
    }
}

// ── Camada 2: Boundary Protection ────────────────────────────────────────────
//
// Verifica se a ação respeita limites de recursos (tokens, tempo, memória).

/// Limites de recursos do sistema.
#[derive(Debug, Clone)]
pub struct BoundaryCheck {
    /// Máximo de tokens permitidos nesta ação.
    pub max_tokens: u64,
    /// Máximo de tempo de execução (ms).
    pub max_time_ms: u64,
    /// Máximo de memória (bytes, aproximado).
    pub max_memory_bytes: u64,
}

impl BoundaryCheck {
    pub fn new(max_tokens: u64, max_time_ms: u64) -> Self {
        Self {
            max_tokens,
            max_time_ms,
            max_memory_bytes: 1024 * 1024 * 512, // 512 MB default
        }
    }

    /// Verifica se os recursos estimados estão dentro dos limites.
    pub fn check(&self, estimated_tokens: u64, estimated_time_ms: u64) -> bool {
        estimated_tokens <= self.max_tokens && estimated_time_ms <= self.max_time_ms
    }

    /// Verifica se há orçamento de tokens restante.
    pub fn has_budget(&self, used: u64) -> bool {
        used < self.max_tokens
    }
}

// ── Camada 3: Operational Norms ──────────────────────────────────────────────
//
// Valida conformidade com regras operacionais (ex: não modificar arquivos
// críticos sem approval, não executar comandos destrutivos).

/// Regra operacional.
#[derive(Debug, Clone)]
pub struct NormRule {
    pub name: String,
    pub description: String,
    pub block_patterns: Vec<String>,
}

/// Conjunto de normas operacionais.
#[derive(Debug, Clone)]
pub struct NormsCheck {
    pub rules: Vec<NormRule>,
}

impl NormsCheck {
    pub fn new() -> Self {
        Self {
            rules: vec![
                NormRule {
                    name: "no-destructive".to_string(),
                    description: "Bloqueia comandos destrutivos".to_string(),
                    block_patterns: vec![
                        "rm -rf".to_string(),
                        "DROP TABLE".to_string(),
                        "chmod 777".to_string(),
                        "format".to_string(),
                    ],
                },
                NormRule {
                    name: "no-credentials".to_string(),
                    description: "Bloqueia exposição de credenciais".to_string(),
                    block_patterns: vec![
                        "api_key".to_string(),
                        "password".to_string(),
                        "token".to_string(),
                    ],
                },
            ],
        }
    }

    /// Verifica se o texto da ação viola alguma norma.
    pub fn check(&self, action_text: &str) -> NormsVerdict {
        let lower = action_text.to_lowercase();
        let violations: Vec<_> = self
            .rules
            .iter()
            .filter(|rule| {
                rule.block_patterns
                    .iter()
                    .any(|p| lower.contains(&p.to_lowercase()))
            })
            .map(|rule| rule.name.clone())
            .collect();

        if violations.is_empty() {
            NormsVerdict::Allow
        } else {
            NormsVerdict::Block { violations }
        }
    }
}

/// Veredito da camada de normas.
#[derive(Debug, Clone, PartialEq)]
pub enum NormsVerdict {
    Allow,
    Block { violations: Vec<String> },
}

impl NormsVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, NormsVerdict::Allow)
    }
}

// ── Camada 4: Feedback Calibration ───────────────────────────────────────────
//
// Ajusta thresholds de confiança baseado em outcomes históricos.
// Se ações similares falharam recentemente, aumenta o limiar.

/// Histórico de feedback para calibração.
#[derive(Debug, Clone)]
pub struct FeedbackCalibration {
    /// Histórico de outcomes por tipo de tarefa: (success_count, total_count)
    pub history: HashMap<String, (u64, u64)>,
    /// Fator de ajuste: quanto reduzir a confiança após falhas.
    pub penalty_factor: f64,
    /// Janela máxima de histórico.
    pub max_history: usize,
}

impl FeedbackCalibration {
    pub fn new() -> Self {
        Self {
            history: HashMap::new(),
            penalty_factor: 0.15,
            max_history: 100,
        }
    }

    /// Registra outcome de uma ação.
    pub fn record(&mut self, task_type: &str, success: bool) {
        let entry = self.history.entry(task_type.to_string()).or_insert((0, 0));
        entry.1 += 1;
        if success {
            entry.0 += 1;
        }
        // Limpa histórico antigo se exceder max_history (simplificado)
        if self.history.len() > self.max_history {
            // Remove entradas com menor total_count
            let mut entries: Vec<_> = self.history.drain().collect();
            entries.sort_by_key(|(_, (_, total))| *total);
            self.history = entries.into_iter().take(self.max_history).collect();
        }
    }

    /// Calcula o threshold ajustado para um tipo de tarefa.
    pub fn adjusted_threshold(&self, task_type: &str, base: f64) -> f64 {
        if let Some((successes, total)) = self.history.get(task_type) {
            if *total == 0 {
                return base;
            }
            let success_rate = *successes as f64 / *total as f64;
            if success_rate < 0.5 {
                // Penalidade: aumenta threshold quando taxa de sucesso é baixa
                (base + self.penalty_factor).min(0.95)
            } else if success_rate > 0.9 {
                // Bônus: reduz threshold quando taxa de sucesso é alta
                (base - self.penalty_factor * 0.5).max(0.60)
            } else {
                base
            }
        } else {
            base
        }
    }
}

// ── Camada 5: Homeostatic Set Point ──────────────────────────────────────────
//
// Mantém variáveis essenciais dentro de faixas seguras (Ashby, 1948).
// Se qualquer variável essencial sair da faixa, força deliberação completa.

/// Variável essencial monitorada.
#[derive(Debug, Clone)]
pub struct EssentialVariable {
    pub name: String,
    pub current: f64,
    pub min_safe: f64,
    pub max_safe: f64,
    pub description: String,
}

/// Monitor de variáveis essenciais (homeostase).
#[derive(Debug, Clone)]
pub struct HomeostaticMonitor {
    pub variables: HashMap<String, EssentialVariable>,
}

impl HomeostaticMonitor {
    pub fn new() -> Self {
        let mut vars = HashMap::new();
        vars.insert(
            "error_rate".to_string(),
            EssentialVariable {
                name: "error_rate".to_string(),
                current: 0.0,
                min_safe: 0.0,
                max_safe: 0.30,
                description: "Taxa de erro nas últimas 100 ações".to_string(),
            },
        );
        vars.insert(
            "token_budget_remaining".to_string(),
            EssentialVariable {
                name: "token_budget_remaining".to_string(),
                current: 1.0,
                min_safe: 0.10,
                max_safe: 1.0,
                description: "Fração do orçamento de tokens restante".to_string(),
            },
        );
        vars.insert(
            "circuit_breaker_health".to_string(),
            EssentialVariable {
                name: "circuit_breaker_health".to_string(),
                current: 1.0,
                min_safe: 0.40,
                max_safe: 1.0,
                description: "Fração de providers saudáveis".to_string(),
            },
        );
        Self { variables: vars }
    }

    /// Atualiza o valor de uma variável essencial.
    pub fn update(&mut self, name: &str, value: f64) {
        if let Some(var) = self.variables.get_mut(name) {
            var.current = value;
        }
    }

    /// Verifica se todas as variáveis essenciais estão dentro dos limites seguros.
    pub fn is_homeostatic(&self) -> bool {
        self.variables
            .values()
            .all(|v| v.current >= v.min_safe && v.current <= v.max_safe)
    }

    /// Retorna lista de variáveis fora da faixa segura.
    pub fn violations(&self) -> Vec<&EssentialVariable> {
        self.variables
            .values()
            .filter(|v| v.current < v.min_safe || v.current > v.max_safe)
            .collect()
    }

    /// Calcula score de saúde do sistema (0.0-1.0).
    pub fn health_score(&self) -> f64 {
        if self.variables.is_empty() {
            return 1.0;
        }
        let scores: Vec<f64> = self
            .variables
            .values()
            .map(|v| {
                if v.current < v.min_safe {
                    v.current / v.min_safe
                } else if v.current > v.max_safe {
                    v.max_safe / v.current
                } else {
                    1.0
                }
            })
            .collect();
        scores.iter().sum::<f64>() / scores.len() as f64
    }
}

// ── Motor IG&C Completo ──────────────────────────────────────────────────────

/// Resultado da avaliação IG&C.
#[derive(Debug, Clone)]
pub struct IgcDecision {
    /// Se true, permite bypass direto para Act sem deliberação.
    pub allow_bypass: bool,
    /// Camada que bloqueou (None se permitido).
    pub blocked_at: Option<String>,
    /// Motivo do bloqueio.
    pub reason: Option<String>,
    /// Score de confiança ajustado.
    pub adjusted_confidence: f64,
    /// Se true, requer deep deliberation (todas as 10 camadas SYMBION).
    pub require_deep_deliberation: bool,
}

/// Motor completo de Implicit Guidance and Control com 5 camadas.
pub struct IgcEngine {
    pub identity: IdentityCheck,
    pub boundary: BoundaryCheck,
    pub norms: NormsCheck,
    pub feedback: FeedbackCalibration,
    pub homeostatic: HomeostaticMonitor,
}

impl IgcEngine {
    pub fn new() -> Self {
        Self {
            identity: IdentityCheck::new(vec![
                "code".to_string(),
                "debug".to_string(),
                "refactor".to_string(),
                "docs".to_string(),
                "test".to_string(),
                "architecture".to_string(),
                "query".to_string(),
                "conversation".to_string(),
            ]),
            boundary: BoundaryCheck::new(100_000, 120_000),
            norms: NormsCheck::new(),
            feedback: FeedbackCalibration::new(),
            homeostatic: HomeostaticMonitor::new(),
        }
    }

    /// Avalia se uma ação pode bypassar a deliberação completa.
    ///
    /// # Parâmetros
    /// - `model`: modelo de orientação da fase Orient.
    /// - `task_type`: tipo de tarefa.
    /// - `action_text`: texto da ação proposta.
    /// - `estimated_tokens`: tokens estimados para a ação.
    /// - `estimated_time_ms`: tempo estimado (ms).
    pub fn evaluate(
        &self,
        model: &OrientationModel,
        task_type: &str,
        action_text: &str,
        estimated_tokens: u64,
        estimated_time_ms: u64,
    ) -> IgcDecision {
        // Se homeostase violada, força deep deliberation
        if !self.homeostatic.is_homeostatic() {
            let violations = self.homeostatic.violations();
            let names: Vec<_> = violations.iter().map(|v| &v.name).collect();
            return IgcDecision {
                allow_bypass: false,
                blocked_at: Some("SETPOINT".to_string()),
                reason: Some(format!(
                    "Variáveis essenciais fora da faixa: {:?}",
                    names
                )),
                adjusted_confidence: model.confidence,
                require_deep_deliberation: true,
            };
        }

        // Camada 1: Identity
        if !self.identity.check(task_type) {
            return IgcDecision {
                allow_bypass: false,
                blocked_at: Some("IDENTITY".to_string()),
                reason: Some(format!(
                    "Tarefa '{}' fora dos domínios permitidos",
                    task_type
                )),
                adjusted_confidence: model.confidence,
                require_deep_deliberation: false,
            };
        }

        // Camada 2: Boundary
        if !self.boundary.check(estimated_tokens, estimated_time_ms) {
            return IgcDecision {
                allow_bypass: false,
                blocked_at: Some("BOUNDARY".to_string()),
                reason: Some(format!(
                    "Recursos estimados excedem limites: {} tokens, {} ms",
                    estimated_tokens, estimated_time_ms
                )),
                adjusted_confidence: model.confidence,
                require_deep_deliberation: false,
            };
        }

        // Camada 3: Norms
        let norms_result = self.norms.check(action_text);
        if !norms_result.is_allowed() {
            if let NormsVerdict::Block { violations } = norms_result {
                return IgcDecision {
                    allow_bypass: false,
                    blocked_at: Some("NORMS".to_string()),
                    reason: Some(format!("Violação de normas: {:?}", violations)),
                    adjusted_confidence: model.confidence,
                    require_deep_deliberation: false,
                };
            }
        }

        // Camada 4: Feedback (ajusta threshold)
        let adjusted_threshold =
            self.feedback
                .adjusted_threshold(task_type, THETA_FEEDBACK);

        // Camada 5: IG&C Bypass decision
        let allow_bypass = model.confidence >= adjusted_threshold;

        IgcDecision {
            allow_bypass,
            blocked_at: if allow_bypass {
                None
            } else {
                Some("CONFIDENCE".to_string())
            },
            reason: if allow_bypass {
                Some("Confiança suficiente para bypass IG&C".to_string())
            } else {
                Some(format!(
                    "Confiança {:.2} abaixo do threshold ajustado {:.2}",
                    model.confidence, adjusted_threshold
                ))
            },
            adjusted_confidence: model.confidence,
            require_deep_deliberation: false,
        }
    }
}

impl Default for IgcEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Camada 1: Identity ──────────────────────────────────────────────

    #[test]
    fn identity_check_allows_known_domain() {
        let check = IdentityCheck::new(vec!["code".to_string()]);
        assert!(check.check("code_generation"));
    }

    #[test]
    fn identity_check_blocks_unknown_domain() {
        let check = IdentityCheck::new(vec!["code".to_string()]);
        assert!(!check.check("crypto_trading"));
    }

    #[test]
    fn identity_alignment_score() {
        let check = IdentityCheck::new(vec!["code".to_string(), "debug".to_string()]);
        assert!((check.alignment_score("code_generation") - 0.50).abs() < 0.01);
    }

    // ── Camada 2: Boundary ──────────────────────────────────────────────

    #[test]
    fn boundary_check_within_limits() {
        let check = BoundaryCheck::new(1000, 60000);
        assert!(check.check(500, 30000));
    }

    #[test]
    fn boundary_check_exceeds_tokens() {
        let check = BoundaryCheck::new(1000, 60000);
        assert!(!check.check(2000, 30000));
    }

    #[test]
    fn boundary_check_exceeds_time() {
        let check = BoundaryCheck::new(1000, 60000);
        assert!(!check.check(500, 120000));
    }

    #[test]
    fn boundary_has_budget() {
        let check = BoundaryCheck::new(1000, 60000);
        assert!(check.has_budget(500));
        assert!(!check.has_budget(1500));
    }

    // ── Camada 3: Norms ─────────────────────────────────────────────────

    #[test]
    fn norms_allows_safe_action() {
        let check = NormsCheck::new();
        assert!(check.check("write file").is_allowed());
    }

    #[test]
    fn norms_blocks_destructive() {
        let check = NormsCheck::new();
        let verdict = check.check("rm -rf /tmp/test");
        assert!(!verdict.is_allowed());
        if let NormsVerdict::Block { violations } = verdict {
            assert!(violations.contains(&"no-destructive".to_string()));
        }
    }

    #[test]
    fn norms_blocks_credentials() {
        let check = NormsCheck::new();
        let verdict = check.check("api_key = 'secret123'");
        assert!(!verdict.is_allowed());
    }

    // ── Camada 4: Feedback ──────────────────────────────────────────────

    #[test]
    fn feedback_adjusts_threshold_on_failure() {
        let mut fb = FeedbackCalibration::new();
        // Registra 1 sucesso, 3 falhas (25% success rate)
        fb.record("code_generation", true);
        fb.record("code_generation", false);
        fb.record("code_generation", false);
        fb.record("code_generation", false);

        let adjusted = fb.adjusted_threshold("code_generation", 0.85);
        assert!(adjusted > 0.85); // Deve aumentar threshold devido a falhas
    }

    #[test]
    fn feedback_reduces_threshold_on_success() {
        let mut fb = FeedbackCalibration::new();
        // 10 sucessos, 0 falhas
        for _ in 0..10 {
            fb.record("quick_query", true);
        }

        let adjusted = fb.adjusted_threshold("quick_query", 0.85);
        assert!(adjusted < 0.85); // Deve reduzir threshold
    }

    #[test]
    fn feedback_unknown_task_keeps_base() {
        let fb = FeedbackCalibration::new();
        let adjusted = fb.adjusted_threshold("new_task", 0.85);
        assert!((adjusted - 0.85).abs() < 0.001);
    }

    // ── Camada 5: Homeostatic Monitor ───────────────────────────────────

    #[test]
    fn homeostatic_all_vars_in_range() {
        let h = HomeostaticMonitor::new();
        assert!(h.is_homeostatic());
    }

    #[test]
    fn homeostatic_error_rate_too_high() {
        let mut h = HomeostaticMonitor::new();
        h.update("error_rate", 0.50); // > max_safe 0.30
        assert!(!h.is_homeostatic());
    }

    #[test]
    fn homeostatic_token_budget_exhausted() {
        let mut h = HomeostaticMonitor::new();
        h.update("token_budget_remaining", 0.05); // < min_safe 0.10
        assert!(!h.is_homeostatic());
    }

    #[test]
    fn homeostatic_violations_list() {
        let mut h = HomeostaticMonitor::new();
        h.update("error_rate", 0.50);
        let violations = h.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].name, "error_rate");
    }

    #[test]
    fn homeostatic_health_score() {
        let mut h = HomeostaticMonitor::new();
        assert!((h.health_score() - 1.0).abs() < 0.01);

        h.update("error_rate", 0.50);
        // error_rate: 0.30/0.50 = 0.60, outras 1.0 → média ≈ 0.87
        let score = h.health_score();
        assert!(score < 1.0);
        assert!(score > 0.70);
    }

    // ── IG&C Engine Integration ─────────────────────────────────────────

    #[test]
    fn igc_bypass_when_confidence_high() {
        let model = OrientationModel {
            confidence: 0.90,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "fast".to_string(),
        };
        assert!(model.should_bypass_decide());
    }

    #[test]
    fn igc_disabled_when_confidence_low() {
        let model = OrientationModel {
            confidence: 0.50,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "slow".to_string(),
        };
        assert!(!model.should_bypass_decide());
    }

    #[test]
    fn igc_engine_blocks_unrecognized_domain() {
        let engine = IgcEngine::new();
        let model = OrientationModel {
            confidence: 0.95,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "fast".to_string(),
        };
        let decision = engine.evaluate(
            &model,
            "crypto_trading",
            "do trade",
            100,
            1000,
        );
        assert!(!decision.allow_bypass);
        assert_eq!(decision.blocked_at, Some("IDENTITY".to_string()));
    }

    #[test]
    fn igc_engine_allows_safe_high_confidence_action() {
        let engine = IgcEngine::new();
        let model = OrientationModel {
            confidence: 0.95,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "fast".to_string(),
        };
        let decision = engine.evaluate(
            &model,
            "code_generation",
            "let x = 42;",
            100,
            1000,
        );
        assert!(decision.allow_bypass);
    }

    #[test]
    fn igc_engine_forces_deep_deliberation_on_homeostatic_violation() {
        let mut engine = IgcEngine::new();
        engine.homeostatic.update("error_rate", 0.90); // Muito acima do safe
        let model = OrientationModel {
            confidence: 0.99,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "fast".to_string(),
        };
        let decision = engine.evaluate(
            &model,
            "code_generation",
            "safe code",
            100,
            1000,
        );
        assert!(!decision.allow_bypass);
        assert!(decision.require_deep_deliberation);
        assert_eq!(decision.blocked_at, Some("SETPOINT".to_string()));
    }

    #[test]
    fn igc_engine_blocks_norm_violation() {
        let engine = IgcEngine::new();
        let model = OrientationModel {
            confidence: 0.99,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "fast".to_string(),
        };
        let decision = engine.evaluate(
            &model,
            "code_generation",
            "DROP TABLE users;",
            100,
            1000,
        );
        assert!(!decision.allow_bypass);
        assert_eq!(decision.blocked_at, Some("NORMS".to_string()));
    }
}
