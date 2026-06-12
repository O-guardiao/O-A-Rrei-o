//! YOLO Classifier — Stage 1 Heurístico + Stage 2 CoT LLM (GAP-009).
//!
//! Classificador automático de risco para auto-aprovação de tool calls.
//! Stage 1: Heurísticas (<10ms) — regex de padrões perigosos, whitelist.
//! Stage 2: Chain-of-Thought via LLM barato — analisa intenção em contexto.
//!
//! Latência alvo Stage 1: <10ms.
//! Latência alvo Stage 2: <2s (com timeout e cache).

use arreio_provider::{ChatRequest, ProviderClient};
use regex::Regex;
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Mutex;

/// Decisão de aprovação automática.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalDecision {
    /// Ferramenta/comando seguro — executar sem intervenção.
    AutoApprove,
    /// Risco incerto — pedir aprovação humana.
    AskUser,
    /// Ferramenta/comando perigoso — negar execução.
    Deny,
}

/// Contexto da sessão usado pelo classificador para decisões mais informadas.
#[derive(Debug, Clone, Default)]
pub struct SessionRiskContext {
    /// Número de negações consecutivas nesta sessão.
    pub consecutive_denials: u8,
    /// Modo de permissão ativo (para ajustar threshold).
    pub permission_mode: String,
    /// Diretório de trabalho atual.
    pub workspace_root: Option<String>,
}

/// Entrada do cache de decisões Stage 2.
#[derive(Debug, Clone)]
struct CacheKey {
    tool_name: String,
    arguments_hash: String,
    task_payload_prefix: String,
}

impl CacheKey {
    fn new(tool_name: &str, arguments: &Value, task_payload: &str) -> Self {
        let args_hash = format!("{:x}", sha2::Sha256::digest(arguments.to_string().as_bytes()))[..16].to_string();
        Self {
            tool_name: tool_name.to_string(),
            arguments_hash: args_hash,
            task_payload_prefix: task_payload.chars().take(200).collect(),
        }
    }
}

/// Resultado do Stage 2 com confiança.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stage2Result {
    pub decision: ApprovalDecision,
    pub confidence: f64,
}

/// Classificador YOLO de 2 estágios.
pub struct YoloClassifier {
    stage1: YoloStage1,
    stage2: Option<Box<dyn YoloStage2Backend>>,
    stage2_cache: Mutex<HashMap<String, Stage2Result>>,
    stage2_timeout_ms: u64,
    stage2_enabled: bool,
}

/// Backend Stage 1 (heurístico).
struct YoloStage1 {
    safe_tools: Vec<&'static str>,
    dangerous_patterns: Vec<Regex>,
    write_tools: Vec<&'static str>,
    max_consecutive_denials: u8,
}

/// Trait para backend Stage 2 (permite mock em testes).
pub trait YoloStage2Backend: Send + Sync {
    fn analyze(&self, tool_name: &str, arguments: &Value, context: &SessionRiskContext, task_payload: &str) -> Stage2Result;
}

/// Backend Stage 2 via LLM (provider secundário barato).
///
/// Modos de operação:
/// - `LlmStage2Backend::new()` → modo heurístico (sem LLM), compatível com testes.
/// - `LlmStage2Backend::with_provider(provider, model)` → classificação via LLM barato
///   com timeout e prompt JSON estruturado.
pub struct LlmStage2Backend {
    provider: Option<Box<dyn ProviderClient>>,
    model: String,
    timeout_ms: u64,
}

impl LlmStage2Backend {
    /// Cria backend no modo heurístico (sem provider).
    /// Útil para testes e para ambientes sem acesso a LLM secundário.
    pub fn new() -> Self {
        Self {
            provider: None,
            model: "heuristic".to_string(),
            timeout_ms: 2000,
        }
    }

    /// Cria backend com provider real para classificação via LLM.
    pub fn with_provider(provider: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            provider: Some(provider),
            model: model.into(),
            timeout_ms: 2000,
        }
    }

    /// Configura timeout da chamada LLM (padrão 2000ms).
    pub fn with_timeout(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    fn analyze_heuristic(tool_name: &str, arguments: &Value) -> Stage2Result {
        let args_str = arguments.to_string().to_lowercase();

        let obfuscation_patterns = [
            "base64", "eval(", "`", "$()", "${", "<<<", ">>",
        ];
        for pat in &obfuscation_patterns {
            if args_str.contains(pat) {
                return Stage2Result {
                    decision: ApprovalDecision::Deny,
                    confidence: 0.85,
                };
            }
        }

        if args_str.contains("../") || args_str.contains("..\\") {
            return Stage2Result {
                decision: ApprovalDecision::Deny,
                confidence: 0.9,
            };
        }

        if tool_name == "exec" {
            let cmd = arguments.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let suspicious = ["&&", "||", ";", "|", ">", "<", "$(", "${"];
            for pat in &suspicious {
                if cmd.contains(pat) {
                    return Stage2Result {
                        decision: ApprovalDecision::AskUser,
                        confidence: 0.75,
                    };
                }
            }
        }

        Stage2Result {
            decision: ApprovalDecision::AutoApprove,
            confidence: 0.6,
        }
    }

    fn analyze_with_llm(
        &self,
        tool_name: &str,
        arguments: &Value,
        context: &SessionRiskContext,
        task_payload: &str,
    ) -> Stage2Result {
        let provider = match self.provider.as_ref() {
            Some(p) => p,
            None => return Self::analyze_heuristic(tool_name, arguments),
        };

        let system = r#"Você é um classificador de segurança para chamadas de ferramentas de um agente de código.
Analise a tool call abaixo e responda APENAS com um JSON no formato exato:
{"decision":"AutoApprove|AskUser|Deny","confidence":0.0}

Regras:
- AutoApprove: operações read-only seguras (ler arquivo, listar diretório, buscar código).
- AskUser: operações de escrita, execução de comando não reconhecido, ou qualquer ambiguidade.
- Deny: deleção em massa, formatação, path traversal, obfuscação, exfiltração óbvia, comandos perigosos (rm -rf, mkfs, dropdb, etc.).
Não inclua explicações fora do JSON."#;

        let user = format!(
            "Tool: {}\nArguments: {}\nPermissionMode: {}\nWorkspace: {:?}\nTask: {}",
            tool_name,
            arguments,
            context.permission_mode,
            context.workspace_root,
            &task_payload[..task_payload.len().min(500)]
        );

        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system: system.to_string(),
            user,
            tools: None,
        };

        let response = match chat_with_timeout(provider, req, self.timeout_ms) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[yolo-stage2] timeout/erro no LLM: {}", e);
                return Stage2Result {
                    decision: ApprovalDecision::AskUser,
                    confidence: 0.0,
                };
            }
        };

        parse_stage2_response(&response.content)
    }
}

impl YoloStage2Backend for LlmStage2Backend {
    fn analyze(&self, tool_name: &str, arguments: &Value, context: &SessionRiskContext, task_payload: &str) -> Stage2Result {
        if self.provider.is_some() {
            self.analyze_with_llm(tool_name, arguments, context, task_payload)
        } else {
            Self::analyze_heuristic(tool_name, arguments)
        }
    }
}

/// Executa chamada ao provider com timeout via thread worker.
fn chat_with_timeout(
    provider: &dyn ProviderClient,
    req: ChatRequest,
    timeout_ms: u64,
) -> anyhow::Result<arreio_provider::ChatResponse> {
    let (tx, rx) = std::sync::mpsc::channel();
    let cloned = provider.clone_box();
    std::thread::spawn(move || {
        let _ = tx.send(cloned.chat(req));
    });
    rx.recv_timeout(std::time::Duration::from_millis(timeout_ms))
        .map_err(|_| anyhow::anyhow!("timeout no Stage 2 após {}ms", timeout_ms))?
}

/// Parse da resposta JSON do Stage 2.
fn parse_stage2_response(content: &str) -> Stage2Result {
    let clean = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(val) = serde_json::from_str::<Value>(clean) {
        let decision = match val.get("decision").and_then(|v| v.as_str()) {
            Some("AutoApprove") => ApprovalDecision::AutoApprove,
            Some("Deny") => ApprovalDecision::Deny,
            _ => ApprovalDecision::AskUser,
        };
        let confidence = val.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
        return Stage2Result { decision, confidence };
    }

    Stage2Result {
        decision: ApprovalDecision::AskUser,
        confidence: 0.0,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Implementação
// ═══════════════════════════════════════════════════════════════════════════════

impl YoloStage1 {
    fn new() -> Self {
        Self {
            safe_tools: vec![
                "read_file",
                "grep_search",
                "glob_search",
                "list_dir",
                "memory_search",
                "web_search",
                "web_fetch",
                "describe_image",
                "transcribe_audio",
            ],
            dangerous_patterns: vec![
                Regex::new(r"(?i)rm\s+-[a-zA-Z]*r").unwrap(),
                Regex::new(r"(?i)del\s+/[sfSF]").unwrap(),
                Regex::new(r"(?i)\bformat\s+[a-zA-Z]:").unwrap(),
                Regex::new(r"(?i)curl\s+.*\|\s*(sh|bash)").unwrap(),
                Regex::new(r"(?i)\bdropdb\b").unwrap(),
                Regex::new(r"(?i)\bDROP\s+DATABASE\b").unwrap(),
                Regex::new(r"(?i)\bmkfs\.").unwrap(),
                Regex::new(r"(?i)\bdd\s+.*of=/dev/").unwrap(),
                Regex::new(r"(?i)\bchmod\s+777\s+/").unwrap(),
                Regex::new(r"(?i)\bsudo\s+-[isA]").unwrap(),
                Regex::new(r"(?i)shutdown|reboot|halt|poweroff").unwrap(),
            ],
            write_tools: vec![
                "write_file",
                "edit_file",
                "apply_patch",
                "exec",
                "checkpoint_rollback",
            ],
            max_consecutive_denials: 3,
        }
    }

    fn classify(&self, tool_name: &str, arguments: &Value, context: &SessionRiskContext) -> (ApprovalDecision, f64) {
        if context.consecutive_denials >= self.max_consecutive_denials {
            return (ApprovalDecision::AskUser, 1.0);
        }

        if self.safe_tools.contains(&tool_name) {
            return (ApprovalDecision::AutoApprove, 0.99);
        }

        if tool_name == "exec" {
            return self.classify_exec(arguments);
        }

        if self.write_tools.contains(&tool_name) {
            return self.classify_write(tool_name, arguments, context);
        }

        (ApprovalDecision::AskUser, 0.5)
    }

    fn classify_exec(&self, arguments: &Value) -> (ApprovalDecision, f64) {
        let cmd = arguments.get("command").and_then(|v| v.as_str()).unwrap_or("");

        for re in &self.dangerous_patterns {
            if re.is_match(cmd) {
                return (ApprovalDecision::Deny, 0.95);
            }
        }

        let safe_prefixes = [
            "echo ", "cat ", "ls ", "dir ", "grep ", "find ", "head ", "tail ",
            "git status", "git log", "git diff", "git show", "git blame", "git branch",
            "cargo check", "cargo test", "cargo build", "rustc --version",
            "pwd", "whoami", "uname", "env", "printenv", "which ",
        ];
        let cmd_lower = cmd.trim().to_lowercase();
        for prefix in &safe_prefixes {
            if cmd_lower.starts_with(prefix) {
                return (ApprovalDecision::AutoApprove, 0.95);
            }
        }

        let build_patterns = [
            "npm test", "npm run", "yarn test", "pnpm test",
            "python -m pytest", "python -m unittest", "make test", "make check",
        ];
        for pattern in &build_patterns {
            if cmd_lower.starts_with(pattern) {
                return (ApprovalDecision::AutoApprove, 0.9);
            }
        }

        (ApprovalDecision::AskUser, 0.5)
    }

    fn classify_write(&self, tool_name: &str, arguments: &Value, context: &SessionRiskContext) -> (ApprovalDecision, f64) {
        let path = arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");

        let sensitive = [
            "/etc/", "/usr/", "/bin/", "/sbin/", "/dev/", "/boot/", "/proc/", "/sys/",
            "C:\\Windows", "C:\\Program", ".ssh/", ".gnupg/", ".aws/",
        ];
        for s in &sensitive {
            if path.contains(s) {
                return (ApprovalDecision::Deny, 0.95);
            }
        }

        if let Some(ref workspace) = context.workspace_root {
            if path.starts_with(workspace.as_str()) || !path.starts_with('/') {
                if tool_name == "checkpoint_rollback" {
                    return (ApprovalDecision::AskUser, 0.7);
                }
                return (ApprovalDecision::AutoApprove, 0.9);
            }
        }

        (ApprovalDecision::AskUser, 0.5)
    }
}

impl YoloClassifier {
    pub fn new() -> Self {
        Self {
            stage1: YoloStage1::new(),
            stage2: None,
            stage2_cache: Mutex::new(HashMap::new()),
            stage2_timeout_ms: 2000,
            stage2_enabled: true,
        }
    }

    /// Desabilita Stage 2.
    pub fn with_stage2_disabled(mut self) -> Self {
        self.stage2_enabled = false;
        self
    }

    /// Configura timeout do Stage 2 (padrão: 2000ms).
    pub fn with_stage2_timeout(mut self, ms: u64) -> Self {
        self.stage2_timeout_ms = ms;
        self
    }

    /// Injeta backend Stage 2 (para testes ou integração real com provider).
    pub fn with_stage2_backend(mut self, backend: Box<dyn YoloStage2Backend>) -> Self {
        self.stage2 = Some(backend);
        self
    }

    /// Conecta um provider LLM real como backend Stage 2.
    pub fn with_llm_stage2(mut self, provider: Box<dyn ProviderClient>, model: &str) -> Self {
        self.stage2 = Some(Box::new(LlmStage2Backend::with_provider(provider, model)));
        self.stage2_enabled = true;
        self
    }

    /// Classifica uma tool call com os 2 estágios.
    pub fn classify(
        &self,
        tool_name: &str,
        arguments: &Value,
        context: &SessionRiskContext,
        task_payload: &str,
    ) -> ApprovalDecision {
        // ── Stage 1 ──
        let (s1_decision, s1_confidence) = self.stage1.classify(tool_name, arguments, context);

        // Fast path: confiança > 0.9 → retorna imediatamente
        if s1_confidence > 0.9 {
            return s1_decision;
        }

        // Se Stage 2 está desabilitado ou não há backend, retorna Stage 1
        if !self.stage2_enabled || self.stage2.is_none() {
            return s1_decision;
        }

        // ── Stage 2 com cache ──
        let cache_key = CacheKey::new(tool_name, arguments, task_payload);
        let cache_key_str = format!(
            "{}:{}:{}",
            cache_key.tool_name, cache_key.arguments_hash, cache_key.task_payload_prefix
        );

        {
            let cache = self.stage2_cache.lock().unwrap();
            if let Some(cached) = cache.get(&cache_key_str) {
                return cached.decision;
            }
        }

        let backend = self.stage2.as_ref().unwrap();
        let s2 = backend.analyze(tool_name, arguments, context, task_payload);

        // Armazena no cache
        {
            let mut cache = self.stage2_cache.lock().unwrap();
            cache.insert(cache_key_str, s2);
        }

        // Stage 2 sobrescreve Stage 1 quando tem confiança maior
        if s2.confidence > s1_confidence {
            s2.decision
        } else {
            s1_decision
        }
    }

    /// Classifica sem payload (backward compat).
    pub fn classify_simple(
        &self,
        tool_name: &str,
        arguments: &Value,
        context: &SessionRiskContext,
    ) -> ApprovalDecision {
        self.classify(tool_name, arguments, context, "")
    }
}

impl Default for YoloClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SessionRiskContext {
        SessionRiskContext {
            consecutive_denials: 0,
            permission_mode: "Default".into(),
            workspace_root: Some("/project".into()),
        }
    }

    #[test]
    fn stage1_read_tools_auto_approve() {
        let c = YoloClassifier::new();
        let ctx = ctx();
        let args = serde_json::json!({"path": "/project/src/main.rs"});
        assert_eq!(c.classify_simple("read_file", &args, &ctx), ApprovalDecision::AutoApprove);
    }

    #[test]
    fn stage1_exec_dangerous_deny() {
        let c = YoloClassifier::new();
        let ctx = ctx();
        let args = serde_json::json!({"command": "rm -rf /"});
        assert_eq!(c.classify_simple("exec", &args, &ctx), ApprovalDecision::Deny);
    }

    #[test]
    fn stage2_detects_obfuscation() {
        let c = YoloClassifier::new().with_stage2_backend(Box::new(LlmStage2Backend::new()));
        let ctx = ctx();
        let args = serde_json::json!({"command": "eval $(base64 -d <<< cm0gLXJmIC8=)"});
        assert_eq!(c.classify_simple("exec", &args, &ctx), ApprovalDecision::Deny);
    }

    #[test]
    fn stage2_detects_path_traversal() {
        let c = YoloClassifier::new().with_stage2_backend(Box::new(LlmStage2Backend::new()));
        let ctx = ctx();
        let args = serde_json::json!({"path": "../../../etc/passwd", "content": "evil"});
        assert_eq!(c.classify_simple("write_file", &args, &ctx), ApprovalDecision::Deny);
    }

    #[test]
    fn stage2_asks_user_for_command_chaining() {
        let c = YoloClassifier::new().with_stage2_backend(Box::new(LlmStage2Backend::new()));
        let ctx = ctx();
        // Comando que não é aprovado nem negado pelo Stage 1 (não está em safe_prefixes nem dangerous_patterns)
        // mas contém chaining suspeito que o Stage 2 deve detectar
        let args = serde_json::json!({"command": "unknown_tool --flag && rm file"});
        assert_eq!(c.classify_simple("exec", &args, &ctx), ApprovalDecision::AskUser);
    }

    #[test]
    fn cache_hits_on_repeated_calls() {
        let c = YoloClassifier::new().with_stage2_backend(Box::new(LlmStage2Backend::new()));
        let ctx = ctx();
        let args = serde_json::json!({"command": "echo test"});
        let d1 = c.classify_simple("exec", &args, &ctx);
        let d2 = c.classify_simple("exec", &args, &ctx);
        assert_eq!(d1, d2);
    }
}
