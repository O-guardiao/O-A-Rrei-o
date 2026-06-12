use anyhow::Result;
use arreio_provider::{ChatRequest, ProviderClient, SnapshotCache, SystemPromptBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;

use crate::prompts::{assemble_system_prompt, ActorRole, SessionState};

// ── Contexto injetado nos atores (sem histórico) ──────────────────────────────

/// Contexto de retry: informa ao Developer que esta é uma retentativa.
/// Populado pelo harness quando o Recovery Block re-executa um nó.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryContext {
    /// Número da tentativa atual (1-based)
    pub attempt_number: u32,
    /// Máximo de tentativas (do error_budget ou recovery block)
    pub max_attempts: u32,
    /// Erros das tentativas anteriores (resumidos)
    pub previous_errors: Vec<String>,
    /// Modelos LLM já tentados (para evitar repetir o mesmo)
    pub models_tried: Vec<String>,
}

/// O único pacote que um ator recebe. Sem histórico conversacional.
/// Inspirado no Tuple Space: cada invocação é stateless, mas rica em contexto
/// curado pelo harness (progressive disclosure, 3 níveis).
pub struct ActorContext {
    // ── Nível 1: Essencial (sempre carregado, ~200 tokens) ──
    pub task_payload: Value,
    pub ast_map: Option<String>,      // JSON compacto de SymbolMap

    // ── Nível 2: Relevante (carregado sob demanda, ~500 tokens) ──
    pub memory_frame: Option<String>, // Frame SIF de memória recuperada
    pub skills_context: String,       // Skills relevantes injetadas pelo SkillMatcher
    pub agents_md: Option<String>,    // Contexto hierárquico AGENTS.md

    // ── Nível 3: Coerência Multi-Passo (NOVO — fecha os 3 gaps) ──
    /// Por que esta tarefa existe. Extraído do raciocínio do Arquiteto
    /// (tupla `dag::rationale` no Blackboard).
    pub architect_rationale: Option<String>,
    /// O que as dependências já fizeram. Resumo dos nós concluídos
    /// (construído pelo harness lendo `dag::node_XXX::result`).
    pub dependencies_summary: Option<String>,
    /// Especificação original (resumida) para manter coerência global.
    /// Ex: "Refatorar auth para OAuth2, mantendo compatibilidade com JWT".
    pub parent_spec: Option<String>,
    /// Contexto de retry — populado apenas quando é uma re-execução.
    pub retry_context: Option<RetryContext>,
    /// Janela de trajetória dos últimos N passos (do TrajectoryStore).
    /// Formato textual compacto, injetado apenas se há histórico relevante.
    pub trajectory_window: Option<String>,
}

// ── Ator Arquiteto ────────────────────────────────────────────────────────────

pub struct Architect {
    client: Box<dyn ProviderClient>,
    model: String,
    cache: RefCell<SnapshotCache>,
}

impl Architect {
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            cache: RefCell::new(SnapshotCache::new()),
        }
    }

    /// Recebe spec em texto, retorna DAG como JSON array.
    pub fn plan(&self, spec: &str) -> Result<Vec<DagTask>> {
        let state = SessionState {
            actor_role: ActorRole::Architect,
            ..Default::default()
        };
        let base = assemble_system_prompt(&state);
        let system = SystemPromptBuilder::new(&mut self.cache.borrow_mut(), "architect", &base)
            .model(&self.model)
            .dynamic(spec)
            .build();
        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system,
            user: spec.into(),
            tools: None,
        };
        let response = self.client.chat(req)?;
        // Extrai bloco JSON da resposta (tolerante a markdown code fences)
        let clean = extract_json_block(&response.content);
        let tasks: Vec<DagTask> = serde_json::from_str(&clean).map_err(|e| {
            anyhow::anyhow!("Arquiteto retornou JSON inválido: {}\n---\n{}", e, clean)
        })?;
        Ok(tasks)
    }
}

// ── Ator Desenvolvedor ────────────────────────────────────────────────────────

pub struct Developer {
    client: Box<dyn ProviderClient>,
    model: String,
    cache: RefCell<SnapshotCache>,
}

impl Developer {
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            cache: RefCell::new(SnapshotCache::new()),
        }
    }

    /// Retorna código gerado (texto puro).
    pub fn code(&self, ctx: &ActorContext) -> Result<String> {
        let user = build_developer_user(ctx);
        let semi = build_developer_semi(ctx);
        let state = SessionState::from_context(ctx, ActorRole::Developer);
        let base = assemble_system_prompt(&state);
        let system = SystemPromptBuilder::new(&mut self.cache.borrow_mut(), "developer", &base)
            .model(&self.model)
            .semi(semi)
            .dynamic(user)
            .build();
        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system,
            user: "".into(), // conteúdo já está no system prompt para cachear prefixo
            tools: None,
        };
        Ok(self.client.chat(req)?.content)
    }
}

fn build_developer_semi(ctx: &ActorContext) -> String {
    let mut parts = Vec::new();
    if !ctx.skills_context.is_empty() {
        parts.push(ctx.skills_context.clone());
    }
    if let Some(agents) = &ctx.agents_md {
        parts.push(format!("## Instruções do Projeto:\n{}", agents));
    }
    // ── Nível 3: Coerência multi-passo (contexto entre atores) ──
    if let Some(rationale) = &ctx.architect_rationale {
        parts.push(format!(
            "## Raciocínio do Arquiteto (por que esta tarefa existe):\n{}",
            rationale
        ));
    }
    if let Some(deps) = &ctx.dependencies_summary {
        parts.push(format!(
            "## O que as dependências já implementaram:\n{}",
            deps
        ));
    }
    if let Some(spec) = &ctx.parent_spec {
        parts.push(format!(
            "## Especificação Original (mantenha coerência com isto):\n{}",
            spec
        ));
    }
    parts.join("\n\n")
}

fn build_developer_user(ctx: &ActorContext) -> String {
    let ast_section = ctx
        .ast_map
        .as_deref()
        .map(|m| {
            format!(
                "\n\n## Mapa AST atual do arquivo (assinaturas only):\n{}",
                m
            )
        })
        .unwrap_or_default();

    let memory_section = ctx
        .memory_frame
        .as_deref()
        .map(|m| format!("\n\n## Memória de Projeto Recuperada:\n{}", m))
        .unwrap_or_default();

    // ── Nível 3: contexto de retry + trajetória ──
    let retry_section = ctx
        .retry_context
        .as_ref()
        .map(|rc| {
            format!(
                "\n\n## ⚠️ RETENTATIVA (tentativa {}/{}): \
                 As tentativas anteriores falharam. NÃO repita a mesma abordagem.\n\
                 Erros anteriores:\n{}\n\
                 Modelos já tentados: {}\n\
                 Ajuste sua estratégia com base nos erros acima.",
                rc.attempt_number,
                rc.max_attempts,
                rc.previous_errors
                    .iter()
                    .map(|e| format!("  - {}", e))
                    .collect::<Vec<_>>()
                    .join("\n"),
                rc.models_tried.join(", ")
            )
        })
        .unwrap_or_default();

    let trajectory_section = ctx
        .trajectory_window
        .as_deref()
        .map(|t| format!("\n\n## Histórico Recente de Execução:\n{}", t))
        .unwrap_or_default();

    format!(
        "## Tarefa:\n{}{}{}{}{}",
        serde_json::to_string_pretty(&ctx.task_payload).unwrap_or_else(|_| "{}".to_string()),
        ast_section,
        memory_section,
        retry_section,
        trajectory_section
    )
}

// ── Ator Inspetor ─────────────────────────────────────────────────────────────

pub struct Inspector {
    client: Box<dyn ProviderClient>,
    model: String,
    cache: RefCell<SnapshotCache>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InspectionResult {
    pub approved: bool,
    pub issues: Vec<String>,
}

/// Metadados da execução do Developer para enriquecer o handoff ao Inspector.
/// Permite que o Inspector avalie não apenas o diff, mas também o processo
/// de geração (ferramentas usadas, tokens, negações de segurança).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeveloperExecutionSummary {
    pub tools_used: Vec<String>,
    pub tools_denied: Vec<String>,
    pub iterations: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_ms: u64,
    pub permission_mode: String,
}

impl Inspector {
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            cache: RefCell::new(SnapshotCache::new()),
        }
    }

    /// Revisa um diff com metadados opcionais da execução do Developer.
    /// O summary permite ao Inspector avaliar o processo, não apenas o resultado.
    pub fn review(&self, diff: &str, summary: Option<&DeveloperExecutionSummary>) -> Result<InspectionResult> {
        let state = SessionState {
            actor_role: ActorRole::Inspector,
            ..Default::default()
        };
        let base = assemble_system_prompt(&state);
        let mut dynamic = diff.to_string();

        if let Some(s) = summary {
            dynamic.push_str("\n\n## Metadados da Execução\n");
            dynamic.push_str(&format!("- Iterações de tool-use: {}\n", s.iterations));
            dynamic.push_str(&format!("- Tokens in/out: {}/{}\n", s.tokens_in, s.tokens_out));
            dynamic.push_str(&format!("- Ferramentas usadas: {}\n", s.tools_used.join(", ")));
            if !s.tools_denied.is_empty() {
                dynamic.push_str(&format!("- Ferramentas negadas: {}\n", s.tools_denied.join(", ")));
            }
            dynamic.push_str(&format!("- Modo de permissão: {}\n", s.permission_mode));
            dynamic.push_str(&format!("- Tempo: {}ms\n", s.duration_ms));
        }

        let system = SystemPromptBuilder::new(&mut self.cache.borrow_mut(), "inspector", &base)
            .model(&self.model)
            .dynamic(&dynamic)
            .build();
        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system,
            user: dynamic,
            tools: None,
        };
        let response = self.client.chat(req)?;
        let clean = extract_json_block(&response.content);
        let result: InspectionResult = serde_json::from_str(&clean).map_err(|e| {
            anyhow::anyhow!("Inspetor retornou JSON inválido: {}\n---\n{}", e, clean)
        })?;
        Ok(result)
    }
}

// ── DAG Task ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DagTask {
    pub id: String,
    pub title: String,
    pub depends_on: Vec<String>,
    pub actor_type: String,
    pub file_target: Option<String>,
    pub instruction: String,
    /// IDs dos contracts DAC aplicáveis a esta tarefa (PVC-Q1.1).
    pub contracts: Vec<String>,
}

// ── Utilitário: extrai JSON de respostas com markdown fences ──────────────────

pub fn extract_json_block(text: &str) -> String {
    // Remove ```json ... ``` ou ``` ... ```
    if let Some(start) = text.find("```json") {
        let inner = &text[start + 7..];
        if let Some(end) = inner.find("```") {
            return inner[..end].trim().to_string();
        }
    }
    if let Some(start) = text.find("```") {
        let inner = &text[start + 3..];
        if let Some(end) = inner.find("```") {
            return inner[..end].trim().to_string();
        }
    }
    // Tenta encontrar [ ou { direto
    let trimmed = text.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return trimmed.to_string();
    }
    // Fallback: retorna como está
    text.trim().to_string()
}

// ── DSML Parser: extrai tool calls serializadas como texto ────────────────────
//
// DeepSeek V4 ocasionalmente (~11% das chamadas multi-turn) serializa tool
// calls como texto no campo `content` usando markup DSML em vez do formato
// estruturado `tool_calls`. Este parser extrai tool calls do texto e as
// converte para o formato estruturado `ToolCall`.

use arreio_provider::{ToolCall, ToolCallFunction};

/// Resultado da extração DSML: texto limpo + tool calls extraídas.
#[derive(Debug, Clone)]
pub struct DsmlResult {
    /// Texto de resposta sem os blocos DSML.
    pub clean_text: String,
    /// Tool calls extraídas do markup DSML.
    pub tool_calls: Vec<ToolCall>,
}

/// Detecta e extrai tool calls em formato DSML do conteúdo textual.
///
/// Formato suportado:
/// ```text
/// <|DSML|tool_calls>
/// <|DSML|invoke name="read_file">
/// <|DSML|parameter name="filePath" string="true">/path/to/file</|DSML|parameter>
/// </|DSML|invoke>
/// <|DSML|invoke name="write_file">
/// <|DSML|parameter name="filePath" string="true">/out.txt</|DSML|parameter>
/// <|DSML|parameter name="content" string="true">Hello</|DSML|parameter>
/// </|DSML|invoke>
/// </|DSML|tool_calls>
/// ```
///
/// E também o formato JSON inline dentro de DSML:
/// ```text
/// <|DSML|tool_calls>
/// [
///   {"name": "read_file", "arguments": {"filePath": "/path/to/file"}}
/// ]
/// </|DSML|tool_calls>
/// ```
pub fn extract_dsml_tool_calls(text: &str) -> DsmlResult {
    // Verifica se há bloco DSML de tool_calls
    let dsml_start = match text.find("<|DSML|tool_calls>") {
        Some(pos) => pos,
        None => {
            return DsmlResult {
                clean_text: text.to_string(),
                tool_calls: Vec::new(),
            };
        }
    };

    let dsml_end = match text[dsml_start..].find("</|DSML|tool_calls>") {
        Some(pos) => dsml_start + pos + "</|DSML|tool_calls>".len(),
        None => {
            // Fallback: tenta extrair JSON inline
            return try_extract_dsml_json_fallback(text, dsml_start);
        }
    };

    let dsml_block = &text[dsml_start..dsml_end];

    // Limpa o texto removendo o bloco DSML
    let clean = format!(
        "{}{}",
        &text[..dsml_start].trim(),
        if dsml_end < text.len() {
            text[dsml_end..].trim()
        } else {
            ""
        }
    );

    // Tenta parse via invoke/parameter tags
    let tool_calls = parse_dsml_invoke_tags(dsml_block);

    let result = if tool_calls.is_empty() {
        // Fallback: tentar JSON inline
        parse_dsml_json(dsml_block)
    } else {
        tool_calls
    };

    DsmlResult {
        clean_text: clean.trim().to_string(),
        tool_calls: result,
    }
}

/// Extrai tool calls via tags `<|DSML|invoke>` e `<|DSML|parameter>`.
fn parse_dsml_invoke_tags(dsml_block: &str) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    let mut call_id = 0u64;

    // Encontra blocos de invoke
    let mut search_from = 0usize;
    while let Some(invoke_start) = dsml_block[search_from..].find("<|DSML|invoke") {
        let abs_start = search_from + invoke_start;
        let after_tag = &dsml_block[abs_start..];

        // Extrai nome da tool
        let name = extract_dsml_attr(after_tag, "name");

        // Encontra o fim do invoke
        let invoke_end = match after_tag.find("</|DSML|invoke>") {
            Some(pos) => abs_start + pos + "</|DSML|invoke>".len(),
            None => {
                search_from = abs_start + 1;
                continue;
            }
        };

        // Extrai parâmetros
        let invoke_body = &dsml_block[abs_start..invoke_end];
        let arguments = parse_dsml_parameters(invoke_body);

        call_id += 1;
        tool_calls.push(ToolCall {
            id: format!("dsml_{}", call_id),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name,
                arguments,
            },
        });

        search_from = invoke_end;
    }

    tool_calls
}

/// Extrai o valor de um atributo XML-like: `name="valor"`.
fn extract_dsml_attr(tag: &str, attr: &str) -> String {
    let pattern = format!("{}=\"", attr);
    if let Some(start) = tag.find(&pattern) {
        let after = &tag[start + pattern.len()..];
        if let Some(end) = after.find('"') {
            return after[..end].to_string();
        }
    }
    String::new()
}

/// Extrai parâmetros de um bloco invoke e monta JSON de arguments.
fn parse_dsml_parameters(invoke_body: &str) -> String {
    let mut params = serde_json::Map::new();
    let mut search_from = 0usize;

    while let Some(param_start) = invoke_body[search_from..].find("<|DSML|parameter") {
        let abs_start = search_from + param_start;
        let after_tag = &invoke_body[abs_start..];

        let name = extract_dsml_attr(after_tag, "name");
        let is_string = extract_dsml_attr(after_tag, "string") == "true";

        // Encontra o valor entre > e </|DSML|parameter>
        let value = if let Some(val_start) = after_tag.find('>') {
            let after_bracket = &after_tag[val_start + 1..];
            if let Some(val_end) = after_bracket.find("</|DSML|parameter>") {
                after_bracket[..val_end].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !name.is_empty() {
            let json_val: Value = if is_string {
                Value::String(value)
            } else {
                // Tenta parsear como número ou bool
                Value::String(value)
            };
            params.insert(name, json_val);
        }

        // Avança busca
        let param_end = abs_start + after_tag.len().min(500); // estimativa segura
        if param_end <= search_from {
            search_from += 1;
        } else {
            search_from = param_end;
        }
    }

    serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string())
}

/// Tenta extrair JSON array de tool calls do bloco DSML.
fn parse_dsml_json(dsml_block: &str) -> Vec<ToolCall> {
    // Procura por [ ... ] dentro do bloco DSML
    let inner = dsml_block
        .trim_start_matches("<|DSML|tool_calls>")
        .trim_end_matches("</|DSML|tool_calls>")
        .trim();

    // Tenta como array de objetos
    if let Ok(arr) = serde_json::from_str::<Vec<Value>>(inner) {
        return arr
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let arguments = v
                    .get("arguments")
                    .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string());
                if name.is_empty() {
                    return None;
                }
                Some(ToolCall {
                    id: format!("dsml_json_{}", i),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.to_string(),
                        arguments,
                    },
                })
            })
            .collect();
    }

    Vec::new()
}

/// Tenta extrair tool calls quando o bloco DSML não tem fechamento esperado.
fn try_extract_dsml_json_fallback(text: &str, dsml_start: usize) -> DsmlResult {
    let after_dsml = &text[dsml_start + "<|DSML|tool_calls>".len()..];
    let trimmed = after_dsml.trim();

    // Tenta parsear como JSON array direto
    if let Ok(arr) = serde_json::from_str::<Vec<Value>>(trimmed) {
        let tool_calls: Vec<ToolCall> = arr
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let arguments = v
                    .get("arguments")
                    .map(|a| {
                        serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string())
                    })
                    .unwrap_or_else(|| "{}".to_string());
                if name.is_empty() {
                    return None;
                }
                Some(ToolCall {
                    id: format!("dsml_fb_{}", i),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.to_string(),
                        arguments,
                    },
                })
            })
            .collect();

        return DsmlResult {
            clean_text: text[..dsml_start].trim().to_string(),
            tool_calls,
        };
    }

    DsmlResult {
        clean_text: text.to_string(),
        tool_calls: Vec::new(),
    }
}

/// Função de conveniência: extrai tool calls do texto da resposta,
/// combinando DSML parser com fallback para padrões conhecidos de
/// ferramentas serializadas como texto (ex: DeepSeek V4 ~11% dos casos).
pub fn extract_tool_calls_from_text(content: &str) -> Option<Vec<ToolCall>> {
    // Primeiro tenta DSML formal
    let dsml = extract_dsml_tool_calls(content);
    if !dsml.tool_calls.is_empty() {
        return Some(dsml.tool_calls);
    }

    // Fallback: padrão "func_name{...JSON...}" comum em tool calls textuais
    // Procura por sequência nome_função{ ... JSON ... }
    if let Some(open_brace) = content.find('{') {
        let before_brace = &content[..open_brace].trim();
        // Pega a última palavra antes da chave como nome da função
        if let Some(func_name) = before_brace.rsplit(|c: char| c.is_whitespace() || c == '\n').next() {
            let func_name = func_name.trim();
            // Verifica se parece nome de função (letras, underscore)
            if !func_name.is_empty()
                && func_name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
            {
                // Tenta extrair JSON balanceado
                let rest = &content[open_brace..];
                let mut depth = 0;
                let mut json_end = 0;
                let mut in_string = false;
                let mut escape = false;

                for (i, c) in rest.char_indices() {
                    if escape {
                        escape = false;
                        continue;
                    }
                    match c {
                        '"' if !escape => in_string = !in_string,
                        '\\' => escape = true,
                        '{' if !in_string => depth += 1,
                        '}' if !in_string => {
                            depth -= 1;
                            if depth == 0 {
                                json_end = i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }

                if json_end > 0 {
                    let args_str = &rest[..json_end];
                    if serde_json::from_str::<Value>(args_str).is_ok() {
                        return Some(vec![ToolCall {
                            id: format!("text_{}", func_name),
                            r#type: "function".to_string(),
                            function: ToolCallFunction {
                                name: func_name.to_string(),
                                arguments: args_str.to_string(),
                            },
                        }]);
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_fence() {
        let text = "Aqui está:\n```json\n[{\"id\":\"t1\"}]\n```\n";
        assert_eq!(extract_json_block(text), "[{\"id\":\"t1\"}]");
    }

    #[test]
    fn extract_bare_json() {
        let text = "[{\"id\":\"t1\"}]";
        assert_eq!(extract_json_block(text), "[{\"id\":\"t1\"}]");
    }

    #[test]
    fn inspection_result_deserialize() {
        let json = r#"{"approved": false, "issues": ["hardcoded password"]}"#;
        let r: InspectionResult = serde_json::from_str(json).unwrap();
        assert!(!r.approved);
        assert_eq!(r.issues.len(), 1);
    }

    // ── Testes DSML ─────────────────────────────────────────────────────

    #[test]
    fn dsml_extract_invoke_tags() {
        let text = r#"Dados ainda incompletos...
<|DSML|tool_calls>
<|DSML|invoke name="read_file">
<|DSML|parameter name="filePath" string="true">/tmp/test.txt</|DSML|parameter>
</|DSML|invoke>
</|DSML|tool_calls>"#;

        let result = extract_dsml_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "read_file");
        assert!(result.tool_calls[0].function.arguments.contains("filePath"));
        assert!(result.tool_calls[0].function.arguments.contains("/tmp/test.txt"));
        assert!(!result.clean_text.contains("DSML"));
        assert!(result.clean_text.contains("Dados ainda incompletos"));
    }

    #[test]
    fn dsml_extract_multiple_invokes() {
        let text = r#"<|DSML|tool_calls>
<|DSML|invoke name="read_file">
<|DSML|parameter name="filePath" string="true">/a.txt</|DSML|parameter>
</|DSML|invoke>
<|DSML|invoke name="write_file">
<|DSML|parameter name="filePath" string="true">/b.txt</|DSML|parameter>
<|DSML|parameter name="content" string="true">Hello</|DSML|parameter>
</|DSML|invoke>
</|DSML|tool_calls>"#;

        let result = extract_dsml_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].function.name, "read_file");
        assert_eq!(result.tool_calls[1].function.name, "write_file");
    }

    #[test]
    fn dsml_json_fallback() {
        let text = r#"<|DSML|tool_calls>
[
  {"name": "read_file", "arguments": {"filePath": "/tmp/test.txt"}},
  {"name": "write_file", "arguments": {"filePath": "/tmp/out.txt", "content": "ok"}}
]
</|DSML|tool_calls>"#;

        let result = extract_dsml_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].function.name, "read_file");
        assert_eq!(result.tool_calls[1].function.name, "write_file");
    }

    #[test]
    fn dsml_no_tool_calls_in_text() {
        let text = "Texto normal sem ferramentas.";
        let result = extract_dsml_tool_calls(text);
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.clean_text, text);
    }

    #[test]
    fn extract_tool_calls_textual_fallback() {
        // Formato comum do DeepSeek V4: nome_da_func{...json...}
        let text = r#"Preciso obter mais informações.
batch_crawl_url_and_answer{"jobs": [{"url": "https://exemplo.com", "questions_to_answer": ["Qual o preço?"]}]}"#;

        let result = extract_tool_calls_from_text(text);
        assert!(result.is_some());
        let tc = result.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "batch_crawl_url_and_answer");
        assert!(tc[0].function.arguments.contains("exemplo.com"));
    }

    #[test]
    fn extract_tool_calls_no_match() {
        let text = "Apenas um texto normal sem tool calls.";
        let result = extract_tool_calls_from_text(text);
        assert!(result.is_none());
    }
}
