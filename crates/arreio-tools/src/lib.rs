//! arreio-tools — Tool Registry, dispatch, search e policy para O Arreio.
//!
//! Traduz o padrão "Tool Plan Builder" do OpenClaw para a arquitetura
//! stateless do Arreio: tools são handlers Rust compilados + adapters MCP.

pub mod critique;
pub mod llm_judge;
pub mod rag;
pub mod skill_crud;
pub mod streaming_executor;

pub use critique::VerifierCritiqueTool;
pub use llm_judge::{CriterionScore, JudgeCriterion, JudgeVerdict, LlmAsJudge};
pub use rag::{ChunkDocumentTool, EmbedTextsTool, VectorSearchTool};

use anyhow::{Context, Result};
use arreio_media::{ImageDescriber, SpeechRecognizer, SpeechSynthesizer};
use arreio_provider::{ToolDescriptor, ToolFunction};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ── Tipos fundamentais ────────────────────────────────────────────────────────

/// Requisição de execução de uma tool.
#[derive(Debug, Clone)]
pub struct ToolRequest {
    pub name: String,
    pub arguments: Value,
}

/// Resultado da execução de uma tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    /// Se a tool foi negada por política de segurança, contém o motivo.
    pub permission_denied: Option<PermissionDenied>,
}

/// Motivo de negação de permissão para uma tool.
#[derive(Debug, Clone)]
pub struct PermissionDenied {
    pub reason: String,
    pub rule_matched: Option<String>,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
            permission_denied: None,
        }
    }
    pub fn err(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            permission_denied: None,
        }
    }
    pub fn denied(reason: impl Into<String>, rule_matched: Option<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: None,
            permission_denied: Some(PermissionDenied {
                reason: reason.into(),
                rule_matched,
            }),
        }
    }
    /// Retorna true se o resultado representa uma negação de permissão.
    pub fn is_permission_denied(&self) -> bool {
        self.permission_denied.is_some()
    }
}

/// Handler de uma tool — implementado por funções nativas ou adapters MCP.
pub trait ToolHandler: Send + Sync {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult>;
}

// ── Tool Registry ─────────────────────────────────────────────────────────────

/// Registro central de tools. Thread-safe via Mutex.
pub struct ToolRegistry {
    handlers: Mutex<HashMap<String, Arc<dyn ToolHandler>>>,
    descriptors: Mutex<HashMap<String, ToolDescriptor>>,
    usage_log: Mutex<HashMap<String, u32>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Mutex::new(HashMap::new()),
            descriptors: Mutex::new(HashMap::new()),
            usage_log: Mutex::new(HashMap::new()),
        }
    }

    /// Registra uma tool com seu handler e descriptor.
    pub fn register(&self, descriptor: ToolDescriptor, handler: Arc<dyn ToolHandler>) {
        let name = descriptor.function.name.clone();
        self.handlers.lock().unwrap().insert(name.clone(), handler);
        self.descriptors
            .lock()
            .unwrap()
            .insert(name.clone(), descriptor);
    }

    /// Executa uma tool pelo nome.
    pub fn call(&self, request: ToolRequest) -> Result<ToolResult> {
        let handler = self
            .handlers
            .lock()
            .unwrap()
            .get(&request.name)
            .cloned()
            .with_context(|| format!("tool '{}' não encontrada", request.name))?;

        // Incrementa contagem de uso
        *self
            .usage_log
            .lock()
            .unwrap()
            .entry(request.name.clone())
            .or_insert(0) += 1;

        handler.handle(request)
    }

    /// Retorna todos os descriptors registrados.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.descriptors.lock().unwrap().values().cloned().collect()
    }

    /// Retorna descriptor de uma tool específica.
    pub fn descriptor(&self, name: &str) -> Option<ToolDescriptor> {
        self.descriptors.lock().unwrap().get(name).cloned()
    }

    /// Busca tools relevantes para uma query (TF-IDF simples).
    pub fn search(&self, query: &str, limit: usize) -> Vec<ToolDescriptor> {
        let descriptors = self.descriptors.lock().unwrap();
        let usage = self.usage_log.lock().unwrap();

        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(f32, ToolDescriptor)> = descriptors
            .values()
            .map(|d| {
                let desc_lower = d.function.description.to_lowercase();
                let name_lower = d.function.name.to_lowercase();

                // Score por matching de palavras
                let mut score = 0.0f32;
                for word in &query_words {
                    if name_lower.contains(word) {
                        score += 3.0;
                    }
                    if desc_lower.contains(word) {
                        score += 1.0;
                    }
                }

                // Bonus por uso frequente (frequência normalizada)
                let use_count = usage.get(&d.function.name).copied().unwrap_or(0);
                score += (use_count as f32).ln_1p() * 0.5;

                (-score, d.clone()) // negativo para ordenar do maior para o menor
            })
            .collect();

        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        scored.into_iter().take(limit).map(|(_, d)| d).collect()
    }

    /// Monta lista de ToolDescriptor para prompt LLM, filtrando por relevância.
    pub fn build_tool_plan(&self, query: &str, max_tools: usize) -> Vec<ToolDescriptor> {
        self.search(query, max_tools)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool Policy Pipeline ──────────────────────────────────────────────────────

/// Política de permissão para execução de tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    Allow,
    Deny,
    Prompt,
}

/// Pipeline de políticas aplicado antes da execução de uma tool.
pub struct ToolPolicyPipeline {
    allowlist: Vec<Regex>,
    denylist: Vec<Regex>,
    mode: PermissionMode,
    security_mode: Option<arreio_security::PermissionModeId>,
    rules: arreio_security::PermissionRules,
    risk_context: arreio_security::SessionRiskContext,
    classifier: arreio_security::YoloClassifier,
    /// Credencial zero-trust do agente (PVC-Q3.2). Quando presente, toda
    /// invocação exige o capability scope `tool:{nome}` — deny-by-default.
    credential: Option<arreio_security::AgentCredential>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    ReadOnly,
    Prompt,
    FullAccess,
}

impl ToolPolicyPipeline {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            allowlist: Vec::new(),
            denylist: Vec::new(),
            mode,
            security_mode: None,
            rules: arreio_security::PermissionRules::new(),
            risk_context: arreio_security::SessionRiskContext::default(),
            classifier: arreio_security::YoloClassifier::new(),
            credential: None,
        }
    }

    /// Cria pipeline usando os 7 modos graduados do crate arreio-security.
    pub fn from_security_mode(mode: arreio_security::PermissionModeId) -> Self {
        Self {
            allowlist: Vec::new(),
            denylist: Vec::new(),
            mode: PermissionMode::FullAccess,
            security_mode: Some(mode),
            rules: arreio_security::PermissionRules::new(),
            risk_context: arreio_security::SessionRiskContext {
                permission_mode: mode.as_str().to_string(),
                ..Default::default()
            },
            classifier: arreio_security::YoloClassifier::new(),
            credential: None,
        }
    }

    /// Anexa a credencial zero-trust do agente (PVC-Q3.2). A partir daqui,
    /// toda invocação de tool exige o scope `tool:{nome}` na credencial.
    pub fn with_credential(mut self, credential: arreio_security::AgentCredential) -> Self {
        self.credential = Some(credential);
        self
    }

    pub fn with_allowlist(mut self, patterns: &[&str]) -> Result<Self> {
        for p in patterns {
            self.allowlist.push(Regex::new(p)?);
        }
        Ok(self)
    }

    pub fn with_denylist(mut self, patterns: &[&str]) -> Result<Self> {
        for p in patterns {
            self.denylist.push(Regex::new(p)?);
        }
        Ok(self)
    }

    pub fn with_rules(mut self, rules: arreio_security::PermissionRules) -> Self {
        self.rules = rules;
        self
    }

    pub fn with_risk_context(mut self, context: arreio_security::SessionRiskContext) -> Self {
        self.risk_context = context;
        self
    }

    pub fn with_classifier(mut self, classifier: arreio_security::YoloClassifier) -> Self {
        self.classifier = classifier;
        self
    }

    /// Avalia se uma tool pode ser executada.
    pub fn authorize(&self, tool_name: &str, arguments: &Value) -> ToolPolicy {
        // Denylist tem prioridade máxima
        for re in &self.denylist {
            if re.is_match(tool_name) {
                return ToolPolicy::Deny;
            }
        }

        // Zero-trust (PVC-Q3.2): credencial é condição NECESSÁRIA, nunca
        // suficiente — concede o gate de scope e o restante do pipeline
        // (regras, modos, classificador) continua valendo.
        if let Some(ref cred) = self.credential {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX); // relógio quebrado → falha fechada
            if cred.is_expired(now) {
                return ToolPolicy::Deny;
            }
            if !cred.authorizes_tool(tool_name) {
                return ToolPolicy::Deny;
            }
        }

        if let Some(decision) = arreio_security::RuleMerger::decide(&self.rules, tool_name, arguments)
        {
            return match decision {
                arreio_security::mergeable_rules::RuleDecision::Allow => ToolPolicy::Allow,
                arreio_security::mergeable_rules::RuleDecision::Ask => ToolPolicy::Prompt,
                arreio_security::mergeable_rules::RuleDecision::Deny => ToolPolicy::Deny,
            };
        }

        // Allowlist (check único, antes dos modos): se configurada, nega
        // imediatamente tudo que não matcha (deny-by-default); se vazia,
        // prossegue aos demais checks (security_mode, ReadOnly, Prompt).
        if !self.allowlist.is_empty() {
            let matched = self.allowlist.iter().any(|re| re.is_match(tool_name));
            if !matched {
                return ToolPolicy::Deny;
            }
        }

        if let Some(mode) = self.security_mode {
            return self.authorize_security_mode(mode, tool_name, arguments);
        }

        // Em modo ReadOnly, apenas tools de leitura são permitidas
        if self.mode == PermissionMode::ReadOnly {
            let read_only_tools = [
                "read_file",
                "grep_search",
                "glob_search",
                "list_dir",
                "memory_search",
                "web_search",
                "web_fetch",
            ];
            if !read_only_tools.contains(&tool_name) {
                return ToolPolicy::Deny;
            }
        }

        // Em modo Prompt, tools destrutivas requerem aprovação
        if self.mode == PermissionMode::Prompt {
            let destructive = [
                "write_file",
                "edit_file",
                "apply_patch",
                "exec",
                "checkpoint_rollback",
            ];
            if destructive.contains(&tool_name) {
                return ToolPolicy::Prompt;
            }
        }

        ToolPolicy::Allow
    }

    fn authorize_security_mode(
        &self,
        mode: arreio_security::PermissionModeId,
        tool_name: &str,
        arguments: &Value,
    ) -> ToolPolicy {
        let spec = arreio_security::PermissionModeSpec::new(mode);
        match spec.authorize(tool_name) {
            arreio_security::ModeAuthorization::Allow => ToolPolicy::Allow,
            arreio_security::ModeAuthorization::Deny => ToolPolicy::Deny,
            arreio_security::ModeAuthorization::Escalate => {
                if mode == arreio_security::PermissionModeId::AutoWithClassifier {
                    match self
                        .classifier
                        .classify(tool_name, arguments, &self.risk_context, "")
                    {
                        arreio_security::ApprovalDecision::AutoApprove => ToolPolicy::Allow,
                        arreio_security::ApprovalDecision::AskUser => ToolPolicy::Prompt,
                        arreio_security::ApprovalDecision::Deny => ToolPolicy::Deny,
                    }
                } else {
                    ToolPolicy::Prompt
                }
            }
        }
    }
}

// ── Concurrency Partitioning (GAP-006) ──────────────────────────────────────

/// Classifica se uma tool é segura para execução concorrente.
/// Tools de leitura pura retornam true; tools com side-effects retornam false.
pub fn is_concurrency_safe(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file"
            | "grep_search"
            | "glob_search"
            | "list_dir"
            | "memory_search"
            | "web_search"
            | "web_fetch"
            | "describe_image"
            | "transcribe_audio"
    )
}

/// Invocação de tool pendente com índice original para preservar ordem.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub index: usize,
    pub name: String,
    pub arguments: Value,
}

/// Resultado de tool com índice original para reordenação.
#[derive(Debug, Clone)]
pub struct IndexedToolResult {
    pub index: usize,
    pub result: ToolResult,
}

/// Particiona invocações em grupos executáveis.
/// Reads consecutivos ficam no mesmo grupo (paralelo); writes isolam grupo (serial).
pub fn partition_invocations(invocations: Vec<ToolInvocation>) -> Vec<Vec<ToolInvocation>> {
    if invocations.is_empty() {
        return vec![];
    }

    let mut groups: Vec<Vec<ToolInvocation>> = Vec::new();
    let mut current_group: Vec<ToolInvocation> = Vec::new();
    let mut current_is_safe = false;

    for inv in invocations {
        let safe = is_concurrency_safe(&inv.name);

        if current_group.is_empty() {
            current_is_safe = safe;
            current_group.push(inv);
        } else if safe && current_is_safe {
            current_group.push(inv);
        } else {
            groups.push(std::mem::take(&mut current_group));
            current_is_safe = safe;
            current_group.push(inv);
        }
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    groups
}

/// Executa um grupo de invocações, paralelizando se todos forem concurrency-safe.
/// Usa crossbeam-channel para coleta de resultados e std::thread::scope para
/// lifetime safety sem requerer Arc.
pub fn execute_group(group: &[ToolInvocation], registry: &ToolRegistry) -> Vec<IndexedToolResult> {
    if group.is_empty() {
        return vec![];
    }

    if group.len() == 1 {
        let inv = &group[0];
        let result = registry
            .call(ToolRequest {
                name: inv.name.clone(),
                arguments: inv.arguments.clone(),
            })
            .unwrap_or_else(|e| ToolResult::err(format!("exec error: {}", e)));
        return vec![IndexedToolResult {
            index: inv.index,
            result,
        }];
    }

    let all_safe = group.iter().all(|inv| is_concurrency_safe(&inv.name));

    if all_safe {
        let (tx, rx) = crossbeam_channel::unbounded();

        std::thread::scope(|s| {
            for inv in group {
                let tx = tx.clone();
                s.spawn(move || {
                    let result = registry
                        .call(ToolRequest {
                            name: inv.name.clone(),
                            arguments: inv.arguments.clone(),
                        })
                        .unwrap_or_else(|e| ToolResult::err(format!("exec error: {}", e)));
                    let _ = tx.send(IndexedToolResult {
                        index: inv.index,
                        result,
                    });
                });
            }
        });

        drop(tx);
        let mut results: Vec<IndexedToolResult> = rx.iter().collect();
        results.sort_by_key(|r| r.index);
        results
    } else {
        group
            .iter()
            .map(|inv| {
                let result = registry
                    .call(ToolRequest {
                        name: inv.name.clone(),
                        arguments: inv.arguments.clone(),
                    })
                    .unwrap_or_else(|e| ToolResult::err(format!("exec error: {}", e)));
                IndexedToolResult {
                    index: inv.index,
                    result,
                }
            })
            .collect()
    }
}

// ── Handlers Nativos ──────────────────────────────────────────────────────────

/// Ler arquivo com limites de tamanho.
pub struct ReadFileHandler;

impl ToolHandler for ReadFileHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let max_size = request
            .arguments
            .get("max_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(1_048_576); // 1MB default

        let meta = fs::metadata(path).with_context(|| format!("lendo metadata de {}", path))?;

        if meta.len() > max_size {
            return Ok(ToolResult::err(format!(
                "arquivo excede limite de {} bytes",
                max_size
            )));
        }

        // Detecção de binário: verifica se há NUL bytes nos primeiros 8KB
        let content = fs::read(path).with_context(|| format!("lendo {}", path))?;
        if content.iter().take(8192).any(|&b| b == 0) {
            return Ok(ToolResult::err("arquivo parece ser binário"));
        }

        let text =
            String::from_utf8(content).with_context(|| format!("{} não é UTF-8 válido", path))?;

        Ok(ToolResult::ok(text))
    }
}

/// Escrever arquivo com workspace boundary.
pub struct WriteFileHandler {
    pub safe_root: PathBuf,
}

impl ToolHandler for WriteFileHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let content = request
            .arguments
            .get("content")
            .and_then(|v| v.as_str())
            .context("argumento 'content' ausente")?;

        let target = Path::new(path);

        // Workspace boundary — resolve para absoluto sem canonicalize (que exige path existente)
        let target_abs = if target.is_absolute() {
            target.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(target)
        };
        let safe_abs = if self.safe_root.is_absolute() {
            self.safe_root.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.safe_root)
        };
        if !target_abs.starts_with(&safe_abs) {
            return Ok(ToolResult::err(format!(
                "path {} fora do workspace seguro {}",
                target_abs.display(),
                safe_abs.display()
            )));
        }

        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }

        fs::write(target, content).with_context(|| format!("escrevendo {}", path))?;

        Ok(ToolResult::ok(format!(
            "arquivo {} escrito ({} bytes)",
            path,
            content.len()
        )))
    }
}

/// Editar arquivo com find/replace.
pub struct EditFileHandler {
    pub safe_root: PathBuf,
}

impl ToolHandler for EditFileHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let old_string = request
            .arguments
            .get("old_string")
            .and_then(|v| v.as_str())
            .context("argumento 'old_string' ausente")?;
        let new_string = request
            .arguments
            .get("new_string")
            .and_then(|v| v.as_str())
            .context("argumento 'new_string' ausente")?;

        let target = Path::new(path);
        let target_abs = if target.is_absolute() {
            target.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(target)
        };
        let safe_abs = if self.safe_root.is_absolute() {
            self.safe_root.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.safe_root)
        };
        if !target_abs.starts_with(&safe_abs) {
            return Ok(ToolResult::err("path fora do workspace seguro".to_string()));
        }

        let content = fs::read_to_string(target).with_context(|| format!("lendo {}", path))?;

        let occurrences = content.matches(old_string).count();
        if occurrences == 0 {
            return Ok(ToolResult::err(format!(
                "string '{}' não encontrada em {}",
                old_string, path
            )));
        }
        if occurrences > 1 {
            return Ok(ToolResult::err(format!(
                "string '{}' ocorre {} vezes em {}. Use apply_patch para edições multi-ocorrência.",
                old_string, occurrences, path
            )));
        }

        let new_content = content.replacen(old_string, new_string, 1);
        fs::write(target, new_content).with_context(|| format!("escrevendo {}", path))?;

        Ok(ToolResult::ok(format!(
            "editado {}: substituído 1 ocorrência",
            path
        )))
    }
}

/// Aplicar patch unificado.
pub struct ApplyPatchHandler {
    pub safe_root: PathBuf,
}

impl ToolHandler for ApplyPatchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let patch = request
            .arguments
            .get("patch")
            .and_then(|v| v.as_str())
            .context("argumento 'patch' ausente")?;

        let target = Path::new(path);
        let target_abs = if target.is_absolute() {
            target.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(target)
        };
        let safe_abs = if self.safe_root.is_absolute() {
            self.safe_root.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.safe_root)
        };
        if !target_abs.starts_with(&safe_abs) {
            return Ok(ToolResult::err("path fora do workspace seguro".to_string()));
        }

        let content = fs::read_to_string(target).with_context(|| format!("lendo {}", path))?;

        // Patch simplificado: linhas que começam com - são removidas, + são adicionadas
        let mut result_lines: Vec<String> = content.lines().map(String::from).collect();
        let patch_lines: Vec<&str> = patch.lines().collect();

        let mut i = 0;
        let mut applied = 0;
        while i < patch_lines.len() {
            if patch_lines[i].starts_with("@@") {
                // Cabeçalho de hunk — ignora no patch simples
                i += 1;
                continue;
            }
            if patch_lines[i].starts_with('-') && !patch_lines[i].starts_with("---") {
                let line_to_remove = &patch_lines[i][1..];
                if let Some(pos) = result_lines
                    .iter()
                    .position(|l| l.trim() == line_to_remove.trim())
                {
                    result_lines.remove(pos);
                    applied += 1;
                }
            } else if patch_lines[i].starts_with('+') && !patch_lines[i].starts_with("+++") {
                let line_to_add = &patch_lines[i][1..];
                result_lines.push(line_to_add.to_string());
                applied += 1;
            }
            i += 1;
        }

        fs::write(target, result_lines.join("\n"))
            .with_context(|| format!("escrevendo {}", path))?;

        Ok(ToolResult::ok(format!(
            "patch aplicado em {} ({} alterações)",
            path, applied
        )))
    }
}

/// Busca regex em arquivos.
pub struct GrepSearchHandler;

impl ToolHandler for GrepSearchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let pattern = request
            .arguments
            .get("pattern")
            .and_then(|v| v.as_str())
            .context("argumento 'pattern' ausente")?;
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let max_results = request
            .arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        let re = Regex::new(pattern).with_context(|| format!("regex inválido: {}", pattern))?;

        let mut results = Vec::new();
        let entries = walkdir::walk(path)?;
        for entry in entries {
            if results.len() >= max_results {
                break;
            }
            let path_str = entry.to_string_lossy();
            if let Ok(content) = fs::read_to_string(&entry) {
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        results.push(format!("{}:{}: {}", path_str, i + 1, line.trim()));
                        if results.len() >= max_results {
                            break;
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(ToolResult::ok("Nenhum resultado encontrado."))
        } else {
            Ok(ToolResult::ok(results.join("\n")))
        }
    }
}

/// Lista diretório.
pub struct ListDirHandler;

impl ToolHandler for ListDirHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let mut entries = Vec::new();
        for entry in fs::read_dir(path).with_context(|| format!("lendo diretório {}", path))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = if entry.file_type()?.is_dir() {
                "dir"
            } else {
                "file"
            };
            entries.push(format!("[{}] {}", file_type, name));
        }

        Ok(ToolResult::ok(entries.join("\n")))
    }
}

/// Glob search (padrão de arquivo).
pub struct GlobSearchHandler;

impl ToolHandler for GlobSearchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let pattern = request
            .arguments
            .get("pattern")
            .and_then(|v| v.as_str())
            .context("argumento 'pattern' ausente")?;
        let max_results = request
            .arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        let mut results = Vec::new();
        let entries = walkdir::walk(".")?;
        let re = Regex::new(&glob_to_regex(pattern))
            .with_context(|| format!("padrão glob inválido: {}", pattern))?;

        for entry in entries {
            if results.len() >= max_results {
                break;
            }
            let path_str = entry.to_string_lossy();
            if re.is_match(&path_str) {
                results.push(path_str.to_string());
            }
        }

        Ok(ToolResult::ok(results.join("\n")))
    }
}

/// Executa comando shell via hypervisor.
pub struct ExecHandler {
    pub safe_root: PathBuf,
    pub timeout_secs: u64,
}

impl ToolHandler for ExecHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let command = request
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .context("argumento 'command' ausente")?;
        let cwd = request.arguments.get("cwd").and_then(|v| v.as_str());

        let work_dir = cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.safe_root.clone());
        let hypervisor = arreio_hypervisor::Hypervisor::new(self.timeout_secs);

        match hypervisor.run(command, Some(&work_dir)) {
            Ok(result) => {
                // Error withholding: bloqueios de permissão retornam como tool_result
                // para o modelo em vez de abortar o loop.
                if result.permission_denied {
                    return Ok(ToolResult::denied(
                        &result.stderr,
                        Some("interceptor".to_string()),
                    ));
                }
                let output = format!(
                    "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                    result.exit_code, result.stdout, result.stderr
                );
                if result.exit_code == 0 {
                    Ok(ToolResult::ok(output))
                } else {
                    Ok(ToolResult::err(output))
                }
            }
            Err(e) => Ok(ToolResult::err(format!("exec error: {}", e))),
        }
    }
}

/// Busca na web (DuckDuckGo).
pub struct WebSearchHandler;

impl ToolHandler for WebSearchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let query = request
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .context("argumento 'query' ausente")?;
        let max_results = request
            .arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;

        match arreio_web::duckduckgo_search(query, max_results) {
            Ok(results) => {
                let mut out = Vec::new();
                for (i, r) in results.iter().enumerate() {
                    out.push(format!(
                        "{}. {}\n   URL: {}\n   {}",
                        i + 1,
                        r.title,
                        r.url,
                        r.snippet
                    ));
                }
                if out.is_empty() {
                    Ok(ToolResult::ok("Nenhum resultado encontrado."))
                } else {
                    Ok(ToolResult::ok(out.join("\n\n")))
                }
            }
            Err(e) => Ok(ToolResult::err(format!("web_search falhou: {}", e))),
        }
    }
}

/// Fetch de página web.
pub struct WebFetchHandler;

impl ToolHandler for WebFetchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let url = request
            .arguments
            .get("url")
            .and_then(|v| v.as_str())
            .context("argumento 'url' ausente")?;
        let max_bytes = request
            .arguments
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(50_000) as usize;

        match arreio_web::web_fetch(url, max_bytes) {
            Ok(text) => Ok(ToolResult::ok(text)),
            Err(e) => Ok(ToolResult::err(format!("web_fetch falhou: {}", e))),
        }
    }
}

/// Busca na memória do Blackboard.
pub struct MemorySearchHandler {
    pub blackboard: arreio_kernel::Blackboard,
}

impl ToolHandler for MemorySearchHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let query = request
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .context("argumento 'query' ausente")?;
        let limit = request
            .arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let results = self.blackboard.search_tuples("memory", query);
        let mut out = Vec::new();
        for (key, value) in results.into_iter().take(limit) {
            out.push(format!(
                "{}: {}",
                key,
                serde_json::to_string_pretty(&value).expect("falha ao serializar valor da memória")
            ));
        }

        Ok(ToolResult::ok(out.join("\n")))
    }
}

/// Escreve na memória do Blackboard.
pub struct MemoryWriteHandler {
    pub blackboard: arreio_kernel::Blackboard,
}

impl ToolHandler for MemoryWriteHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let key = request
            .arguments
            .get("key")
            .and_then(|v| v.as_str())
            .context("argumento 'key' ausente")?;
        let value = request
            .arguments
            .get("value")
            .cloned()
            .context("argumento 'value' ausente")?;

        self.blackboard.put_tuple("memory", key, value)?;
        Ok(ToolResult::ok(format!("memória '{}' escrita", key)))
    }
}

/// Cria checkpoint git.
pub struct CheckpointSaveHandler;

impl ToolHandler for CheckpointSaveHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let node_id = request
            .arguments
            .get("node_id")
            .and_then(|v| v.as_str())
            .context("argumento 'node_id' ausente")?;
        let work_dir = request
            .arguments
            .get("work_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        arreio_dag::Checkpoint::save(node_id, &PathBuf::from(work_dir))?;
        Ok(ToolResult::ok(format!("checkpoint salvo para {}", node_id)))
    }
}

/// Rollback para checkpoint git.
pub struct CheckpointRollbackHandler;

impl ToolHandler for CheckpointRollbackHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let work_dir = request
            .arguments
            .get("work_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        arreio_dag::Checkpoint::rollback(&PathBuf::from(work_dir))?;
        Ok(ToolResult::ok("rollback executado".to_string()))
    }
}

/// Tool nativa para gerenciar todo items.
pub struct TodoHandler {
    pub blackboard: arreio_kernel::Blackboard,
    pub session_id: String,
}

impl ToolHandler for TodoHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let action = request
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .context("argumento 'action' ausente")?;
        let store = arreio_dag::TodoStore::new(self.blackboard.clone(), &self.session_id);

        match action {
            "create" => {
                let id = request
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .context("argumento 'id' ausente")?;
                let content = request
                    .arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .context("argumento 'content' ausente")?;
                let item = store.create(id, content)?;
                Ok(ToolResult::ok(format!(
                    "Todo criado: {} - {}",
                    item.id, item.content
                )))
            }
            "update" => {
                let id = request
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .context("argumento 'id' ausente")?;
                let content = request
                    .arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let status = request
                    .arguments
                    .get("status")
                    .and_then(|v| v.as_str())
                    .and_then(|s| match s {
                        "pending" => Some(arreio_dag::TodoStatus::Pending),
                        "in_progress" => Some(arreio_dag::TodoStatus::InProgress),
                        "completed" => Some(arreio_dag::TodoStatus::Completed),
                        "cancelled" => Some(arreio_dag::TodoStatus::Cancelled),
                        _ => None,
                    });
                let item = store.update(id, content, status)?;
                Ok(ToolResult::ok(format!(
                    "Todo atualizado: {} - {:?}",
                    item.id, item.status
                )))
            }
            "list" => {
                let items = store.list();
                let (pending, in_progress, completed, cancelled, total) = store.kanban_summary();
                let mut out = vec![format!(
                    "Todos: {}/{}/{} (pending/in_progress/completed) | cancelled: {} | total: {}",
                    pending, in_progress, completed, cancelled, total
                )];
                for item in items {
                    out.push(format!(
                        "- [{}] {}: {}",
                        item.id,
                        format_status(&item.status),
                        item.content
                    ));
                }
                Ok(ToolResult::ok(out.join("\n")))
            }
            "complete" => {
                let id = request
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .context("argumento 'id' ausente")?;
                let item = store.complete(id)?;
                Ok(ToolResult::ok(format!("Todo completado: {}", item.id)))
            }
            "cancel" => {
                let id = request
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .context("argumento 'id' ausente")?;
                let item = store.cancel(id)?;
                Ok(ToolResult::ok(format!("Todo cancelado: {}", item.id)))
            }
            _ => Ok(ToolResult::err(format!("ação desconhecida: {}", action))),
        }
    }
}

fn format_status(status: &arreio_dag::TodoStatus) -> &'static str {
    match status {
        arreio_dag::TodoStatus::Pending => "PENDING",
        arreio_dag::TodoStatus::InProgress => "IN_PROGRESS",
        arreio_dag::TodoStatus::Completed => "COMPLETED",
        arreio_dag::TodoStatus::Cancelled => "CANCELLED",
    }
}

// ── Media Handlers ────────────────────────────────────────────────────────────

/// Descreve imagem via modelo vision (Ollama).
pub struct DescribeImageHandler;

impl ToolHandler for DescribeImageHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let prompt = request.arguments.get("prompt").and_then(|v| v.as_str());

        let describer = arreio_media::OllamaVisionDescriber::new("llava");
        match describer.describe(Path::new(path), prompt) {
            Ok(desc) => Ok(ToolResult::ok(desc)),
            Err(e) => Ok(ToolResult::err(format!("vision falhou: {}", e))),
        }
    }
}

/// Sintetiza fala via espeak-ng.
pub struct SynthesizeSpeechHandler;

impl ToolHandler for SynthesizeSpeechHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let text = request
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .context("argumento 'text' ausente")?;
        let language = request.arguments.get("language").and_then(|v| v.as_str());
        let output_name = request
            .arguments
            .get("output_name")
            .and_then(|v| v.as_str())
            .unwrap_or("speech.wav");

        let tts = arreio_media::EspeakTts::new();
        if !tts.is_available() {
            return Ok(ToolResult::err(
                "TTS não disponível. Instale espeak-ng: pacman -S mingw-w64-ucrt-x86_64-espeak-ng"
                    .to_string(),
            ));
        }

        match tts.synthesize(text, language) {
            Ok(result) => {
                let path = arreio_media::save_media(&result.audio_bytes, output_name)?;
                Ok(ToolResult::ok(format!(
                    "Áudio sintetizado: {} ({} bytes, ~{:.1}s)",
                    path.display(),
                    result.audio_bytes.len(),
                    result.duration_secs.unwrap_or(0.0)
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("TTS falhou: {}", e))),
        }
    }
}

/// Transcreve áudio via whisper.
pub struct TranscribeAudioHandler;

impl ToolHandler for TranscribeAudioHandler {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let path = request
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .context("argumento 'path' ausente")?;
        let language = request.arguments.get("language").and_then(|v| v.as_str());

        let stt = arreio_media::WhisperStt::new();
        if !stt.is_available() {
            return Ok(ToolResult::err(
                "STT não disponível. Instale whisper ou whisper-cli.".to_string(),
            ));
        }

        match stt.transcribe(Path::new(path), language) {
            Ok(result) => Ok(ToolResult::ok(format!(
                "Transcrição ({}): {}",
                result.language, result.text
            ))),
            Err(e) => Ok(ToolResult::err(format!("STT falhou: {}", e))),
        }
    }
}

// ── MCP Tool Adapter ──────────────────────────────────────────────────────────

/// Adapta um servidor MCP como handler de tool.
pub struct McpToolAdapter {
    client: Arc<Mutex<arreio_mcp::McpClient>>,
    tool_name: String,
}

impl McpToolAdapter {
    pub fn new(client: Arc<Mutex<arreio_mcp::McpClient>>, tool_name: impl Into<String>) -> Self {
        Self {
            client,
            tool_name: tool_name.into(),
        }
    }
}

impl ToolHandler for McpToolAdapter {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let mut client = self.client.lock().unwrap();
        let result = client.call_tool(&self.tool_name, request.arguments)?;

        let text = result
            .content
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error {
            Ok(ToolResult::err(text))
        } else {
            Ok(ToolResult::ok(text))
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Converte padrão glob simples para regex.
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' => regex.push_str("\\."),
            '+' => regex.push_str("\\+"),
            '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => regex.push(ch),
        }
    }
    regex.push('$');
    regex
}

/// Walk recursivo simples (sem dependência externa).
mod walkdir {
    use std::path::PathBuf;

    pub fn walk(dir: &str) -> anyhow::Result<Vec<PathBuf>> {
        let mut results = Vec::new();
        let mut stack = vec![PathBuf::from(dir)];
        while let Some(current) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&current) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        results.push(path);
                    }
                }
            }
        }
        Ok(results)
    }
}

// ── Builder de descriptors padrão ─────────────────────────────────────────────

/// Cria descriptors para todas as tools nativas do Arreio.
pub fn build_native_tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "read_file".to_string(),
                description: "Read the contents of a file. Use this to examine source code, configs, or documentation.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or relative path to the file" },
                        "max_size": { "type": "integer", "description": "Max bytes to read (default 1MB)" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "write_file".to_string(),
                description: "Write content to a file. Creates parent directories if needed.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" },
                        "content": { "type": "string", "description": "Content to write" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "edit_file".to_string(),
                description: "Replace a single occurrence of old_string with new_string in a file. Fails if multiple occurrences exist.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "apply_patch".to_string(),
                description: "Apply a unified diff patch to a file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "patch": { "type": "string", "description": "Unified diff text" }
                    },
                    "required": ["path", "patch"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "grep_search".to_string(),
                description: "Search files using regex pattern. Returns matching lines with file path and line number.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern" },
                        "path": { "type": "string", "description": "Directory to search (default .)" },
                        "max_results": { "type": "integer", "description": "Max results (default 50)" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "glob_search".to_string(),
                description: "Find files matching a glob pattern (e.g. '*.rs', 'src/**/*.toml').".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "max_results": { "type": "integer", "description": "Max results (default 100)" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "list_dir".to_string(),
                description: "List files and directories in a path.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path (default .)" }
                    }
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "exec".to_string(),
                description: "Execute a shell command. Use with caution.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" },
                        "cwd": { "type": "string", "description": "Working directory (default workspace root)" }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "web_search".to_string(),
                description: "Search the web using DuckDuckGo. Returns top results with title, URL and snippet.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "max_results": { "type": "integer", "description": "Max results (default 3)" }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "web_fetch".to_string(),
                description: "Fetch a web page and extract readable text.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "URL to fetch" },
                        "max_bytes": { "type": "integer", "description": "Max bytes to fetch (default 50000)" }
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "memory_search".to_string(),
                description: "Search the Blackboard memory tuple space.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "description": "Max results (default 5)" }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "memory_write".to_string(),
                description: "Write a value to the Blackboard memory tuple space.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "value": { "type": "object", "description": "Any JSON value" }
                    },
                    "required": ["key", "value"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "checkpoint_save".to_string(),
                description: "Create a git checkpoint for the current state.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "work_dir": { "type": "string" }
                    },
                    "required": ["node_id"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "checkpoint_rollback".to_string(),
                description: "Rollback to the last git checkpoint.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "work_dir": { "type": "string" }
                    }
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "describe_image".to_string(),
                description: "Describe the contents of an image file using a vision model.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the image file" },
                        "prompt": { "type": "string", "description": "Optional custom prompt for the vision model" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "synthesize_speech".to_string(),
                description: "Convert text to speech and save as a WAV file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Text to synthesize" },
                        "language": { "type": "string", "description": "Language code (e.g. pt, en). Optional." },
                        "output_name": { "type": "string", "description": "Output filename (default: speech.wav)" }
                    },
                    "required": ["text"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "transcribe_audio".to_string(),
                description: "Transcribe an audio file to text using speech recognition.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the audio file" },
                        "language": { "type": "string", "description": "Language code (e.g. pt, en). Optional." }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "todo".to_string(),
                description: "Manage todo items: create, update, list, complete, cancel.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["create", "update", "list", "complete", "cancel"], "description": "Action to perform" },
                        "id": { "type": "string", "description": "Todo item ID (required for create/update/complete/cancel)" },
                        "content": { "type": "string", "description": "Todo content (required for create/update)" },
                        "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"], "description": "Status for update action" }
                    },
                    "required": ["action"]
                }),
            },
        },
    ]
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn registry_register_and_call() {
        let registry = ToolRegistry::new();
        let desc = ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "test_echo".to_string(),
                description: "Echo test".to_string(),
                parameters: serde_json::json!({}),
            },
        };
        struct EchoHandler;
        impl ToolHandler for EchoHandler {
            fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
                Ok(ToolResult::ok(format!(
                    "echo: {}",
                    request
                        .arguments
                        .get("msg")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                )))
            }
        }
        registry.register(desc, Arc::new(EchoHandler));

        let result = registry
            .call(ToolRequest {
                name: "test_echo".to_string(),
                arguments: serde_json::json!({"msg": "hello"}),
            })
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[test]
    fn read_file_handler_works() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "conteúdo de teste").unwrap();

        let handler = ReadFileHandler;
        let result = handler
            .handle(ToolRequest {
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": path.to_string_lossy()}),
            })
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("conteúdo de teste"));
    }

    #[test]
    fn write_file_handler_respects_boundary() {
        let safe = TempDir::new().unwrap();
        let handler = WriteFileHandler {
            safe_root: safe.path().to_path_buf(),
        };

        // Escrita dentro do safe root deve funcionar
        let target = safe.path().join("sub/dir/file.txt");
        let result = handler
            .handle(ToolRequest {
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "hello"
                }),
            })
            .unwrap();
        assert!(result.success);

        // Escrita fora deve falhar
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("evil.txt");
        let result2 = handler
            .handle(ToolRequest {
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": outside_file.to_string_lossy(),
                    "content": "evil"
                }),
            })
            .unwrap();
        assert!(!result2.success);
    }

    #[test]
    fn policy_readonly_blocks_write() {
        let policy = ToolPolicyPipeline::new(PermissionMode::ReadOnly);
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({})),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn policy_prompt_for_destructive() {
        let policy = ToolPolicyPipeline::new(PermissionMode::Prompt);
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({})),
            ToolPolicy::Prompt
        );
    }

    // ── PVC-Q3.2: credencial zero-trust scoped a invocation ─────────────────

    const CRED_SECRET: &str = "a-very-secure-secret-key-for-testing-32chars";

    fn credential_with_scopes(scopes: &[&str]) -> arreio_security::AgentCredential {
        let token = arreio_security::AgentCredential::issue_with_secret(
            "agent-test",
            "developer",
            scopes,
            1,
            CRED_SECRET,
        )
        .unwrap();
        arreio_security::AgentCredential::verify_with_secret(&token, CRED_SECRET).unwrap()
    }

    #[test]
    fn credencial_sem_scope_nega_tool() {
        let cred = credential_with_scopes(&["tool:read_file"]);
        let policy =
            ToolPolicyPipeline::new(PermissionMode::FullAccess).with_credential(cred);
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
        // write_file não está nos scopes → deny-by-default
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({})),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn credencial_e_necessaria_mas_nao_suficiente() {
        // Scope concede write_file, mas o modo ReadOnly do pipeline ainda nega.
        let cred = credential_with_scopes(&["tool:*"]);
        let policy = ToolPolicyPipeline::new(PermissionMode::ReadOnly).with_credential(cred);
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({})),
            ToolPolicy::Deny
        );
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
    }

    #[test]
    fn credencial_expirada_nega_tudo() {
        let mut cred = credential_with_scopes(&["tool:*"]);
        cred.expires_at = 1; // passado distante
        let policy =
            ToolPolicyPipeline::new(PermissionMode::FullAccess).with_credential(cred);
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn denylist_vence_mesmo_com_scope() {
        let cred = credential_with_scopes(&["tool:*"]);
        let policy = ToolPolicyPipeline::new(PermissionMode::FullAccess)
            .with_denylist(&["^exec$"])
            .unwrap()
            .with_credential(cred);
        assert_eq!(
            policy.authorize("exec", &serde_json::json!({})),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn sem_credencial_pipeline_funciona_como_antes() {
        let policy = ToolPolicyPipeline::new(PermissionMode::FullAccess);
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
    }

    #[test]
    fn policy_security_mode_default_escalates_writes() {
        let policy =
            ToolPolicyPipeline::from_security_mode(arreio_security::PermissionModeId::Default);
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({})),
            ToolPolicy::Allow
        );
        assert_eq!(
            policy.authorize("write_file", &serde_json::json!({"path": "src/lib.rs"})),
            ToolPolicy::Prompt
        );
    }

    #[test]
    fn policy_rules_deny_win_before_mode() {
        let rules = arreio_security::PermissionRules {
            deny: vec![arreio_security::PermissionRule::parse(
                "read_file",
                arreio_security::RuleScope::Project,
            )
            .unwrap()],
            ..arreio_security::PermissionRules::new()
        };
        let policy = ToolPolicyPipeline::from_security_mode(arreio_security::PermissionModeId::Auto)
            .with_rules(rules);

        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({"path": "src/lib.rs"})),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn policy_auto_classifier_uses_yolo_decision() {
        let policy = ToolPolicyPipeline::from_security_mode(
            arreio_security::PermissionModeId::AutoWithClassifier,
        )
        .with_risk_context(arreio_security::SessionRiskContext {
            workspace_root: Some(".".into()),
            ..Default::default()
        });

        assert_eq!(
            policy.authorize("exec", &serde_json::json!({"command": "rm -rf /"})),
            ToolPolicy::Deny
        );
        assert_eq!(
            policy.authorize("read_file", &serde_json::json!({"path": "src/lib.rs"})),
            ToolPolicy::Allow
        );
    }

    #[test]
    fn search_ranks_by_relevance() {
        let registry = ToolRegistry::new();
        for i in 0..5 {
            let desc = ToolDescriptor {
                r#type: "function".to_string(),
                function: ToolFunction {
                    name: format!("tool_{}", i),
                    description: format!(
                        "Tool for {} operations",
                        if i == 2 { "file reading" } else { "other" }
                    ),
                    parameters: serde_json::json!({}),
                },
            };
            struct Dummy;
            impl ToolHandler for Dummy {
                fn handle(&self, _r: ToolRequest) -> Result<ToolResult> {
                    Ok(ToolResult::ok(""))
                }
            }
            registry.register(desc, Arc::new(Dummy));
        }

        let results = registry.search("file read", 3);
        assert_eq!(results[0].function.name, "tool_2"); // melhor match
    }

    #[test]
    fn tool_result_denied_has_permission_denied_flag() {
        let res = ToolResult::denied(
            "comando bloqueado pelo interceptor",
            Some("rm_rf".to_string()),
        );
        assert!(!res.success);
        assert!(res.is_permission_denied());
        assert_eq!(
            res.permission_denied.as_ref().unwrap().reason,
            "comando bloqueado pelo interceptor"
        );
        assert_eq!(
            res.permission_denied.as_ref().unwrap().rule_matched,
            Some("rm_rf".to_string())
        );
    }

    #[test]
    fn tool_result_ok_is_not_denied() {
        let res = ToolResult::ok("output");
        assert!(res.success);
        assert!(!res.is_permission_denied());
    }

    #[test]
    fn tool_result_err_is_not_denied() {
        let res = ToolResult::err("erro genérico");
        assert!(!res.success);
        assert!(!res.is_permission_denied());
    }

    // ── GAP-006: Concurrency Partitioning ───────────────────────────────────

    #[test]
    fn concurrency_safe_classifica_tools() {
        assert!(is_concurrency_safe("read_file"));
        assert!(is_concurrency_safe("grep_search"));
        assert!(is_concurrency_safe("glob_search"));
        assert!(is_concurrency_safe("list_dir"));
        assert!(is_concurrency_safe("memory_search"));
        assert!(is_concurrency_safe("web_search"));
        assert!(is_concurrency_safe("web_fetch"));
        assert!(!is_concurrency_safe("write_file"));
        assert!(!is_concurrency_safe("edit_file"));
        assert!(!is_concurrency_safe("apply_patch"));
        assert!(!is_concurrency_safe("exec"));
        assert!(!is_concurrency_safe("checkpoint_rollback"));
    }

    #[test]
    fn partition_agrupa_reads_e_isola_writes() {
        let invocations = vec![
            ToolInvocation {
                index: 0,
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 1,
                name: "grep_search".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 2,
                name: "write_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 3,
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 4,
                name: "list_dir".into(),
                arguments: serde_json::json!({}),
            },
        ];

        let groups = partition_invocations(invocations);
        assert_eq!(groups.len(), 3, "deve ter 3 grupos: reads, write, reads");
        assert_eq!(groups[0].len(), 2, "primeiro grupo: 2 reads");
        assert_eq!(groups[1].len(), 1, "segundo grupo: 1 write");
        assert_eq!(groups[2].len(), 2, "terceiro grupo: 2 reads");
        assert_eq!(groups[0][0].name, "read_file");
        assert_eq!(groups[0][1].name, "grep_search");
        assert_eq!(groups[1][0].name, "write_file");
        assert_eq!(groups[2][0].name, "read_file");
        assert_eq!(groups[2][1].name, "list_dir");
    }

    #[test]
    fn partition_vazio_retorna_vazio() {
        let groups = partition_invocations(vec![]);
        assert!(groups.is_empty());
    }

    #[test]
    fn partition_unico_write_retorna_grupo_unico() {
        let invocations = vec![ToolInvocation {
            index: 0,
            name: "exec".into(),
            arguments: serde_json::json!({}),
        }];
        let groups = partition_invocations(invocations);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 1);
    }

    #[test]
    fn partition_writes_consecutivos_ficam_isolados() {
        let invocations = vec![
            ToolInvocation {
                index: 0,
                name: "write_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 1,
                name: "edit_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 2,
                name: "exec".into(),
                arguments: serde_json::json!({}),
            },
        ];
        let groups = partition_invocations(invocations);
        assert_eq!(groups.len(), 3, "cada write isolado em seu grupo");
    }

    #[test]
    fn execute_group_preserva_ordem() {
        let registry = ToolRegistry::new();
        struct NameEchoHandler;
        impl ToolHandler for NameEchoHandler {
            fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
                std::thread::sleep(std::time::Duration::from_millis(5));
                Ok(ToolResult::ok(format!("resultado de {}", request.name)))
            }
        }

        for name in &["read_file", "grep_search", "list_dir"] {
            let desc = ToolDescriptor {
                r#type: "function".to_string(),
                function: ToolFunction {
                    name: name.to_string(),
                    description: "test".to_string(),
                    parameters: serde_json::json!({}),
                },
            };
            registry.register(desc, Arc::new(NameEchoHandler));
        }

        let group = vec![
            ToolInvocation {
                index: 5,
                name: "list_dir".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 2,
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolInvocation {
                index: 8,
                name: "grep_search".into(),
                arguments: serde_json::json!({}),
            },
        ];

        let results = execute_group(&group, &registry);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].index, 2);
        assert_eq!(results[1].index, 5);
        assert_eq!(results[2].index, 8);
        assert!(results[0].result.success);
        assert!(results[1].result.success);
        assert!(results[2].result.success);
    }
}
