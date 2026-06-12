//! A2ATaskDispatcher — executa tasks detectadas automaticamente dentro do chat.
//!
//! Quando o `IntentClassifier` detecta uma task, este dispatcher:
//!   1. Monta um contexto de execução a partir da mensagem do usuário
//!   2. Executa o Developer com tool-use
//!   3. Envia mensagens de progresso ao chat
//!   4. Retorna o resultado como mensagem do assistente
//!
//! O usuário nunca vê DAG, FSM, checkpoints ou rollback.
//! Vê apenas: "Vou criar isso para você..." → "✅ Pronto!"

use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use arreio_provider::{ChatRequest, ProviderClient};
use arreio_tools::{PermissionMode, ToolPolicyPipeline, ToolRegistry, ToolRequest};

use std::path::PathBuf;

/// Resultado da execução de uma task.
#[derive(Debug, Clone)]
pub struct TaskDispatchResult {
    /// Sucesso ou falha.
    pub success: bool,
    /// Mensagem para o usuário (resultado ou erro amigável).
    pub message: String,
    /// Arquivos criados/modificados.
    pub files: Vec<String>,
    /// Comando de validação executado (se houver).
    pub validation_cmd: Option<String>,
    /// Exit code da validação.
    pub validation_exit_code: i32,
}

/// Dispatcher de tasks para execução automática dentro do chat.
pub struct A2ATaskDispatcher {
    blackboard: Blackboard,
    work_dir: PathBuf,
}

impl A2ATaskDispatcher {
    /// Cria um novo dispatcher.
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            work_dir: PathBuf::from("."),
        }
    }

    /// Define o diretório de trabalho.
    pub fn with_work_dir(mut self, dir: PathBuf) -> Self {
        self.work_dir = dir;
        self
    }

    /// Executa uma task a partir da mensagem do usuário.
    ///
    /// Retorna `TaskDispatchResult` com o resultado formatado para o chat.
    pub fn execute(
        &self,
        user_message: &str,
        provider: &dyn ProviderClient,
        model: &str,
    ) -> Result<TaskDispatchResult> {
        let mut progress = Vec::new();
        progress.push("⏳ Analisando sua solicitação...".to_string());

        // 1. Monta system prompt para execução de task
        let system_prompt = format!(
            "Você é um assistente de execução de tarefas. O usuário pediu algo para ser feito. \
Execute a tarefa usando as ferramentas disponíveis (ler arquivo, escrever arquivo, buscar, etc.). \
Quando precisar modificar um arquivo, primeiro leia-o, depois escreva a versão completa. \
Retorne APENAS o resultado final da tarefa, em linguagem natural amigável. \
Se criou arquivos, liste-os. Se houve erro, explique de forma simples."
        );

        // 2. Registry de tools nativas
        let registry = self.build_tool_registry()?;
        let descriptors = registry.build_tool_plan(user_message, 8);

        if !descriptors.is_empty() {
            progress.push(format!(
                "🔍 Ferramentas selecionadas: {}",
                descriptors
                    .iter()
                    .map(|d| d.function.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // 3. Tool-use loop (simplificado do cmd_run)
        let mut tool_history = String::new();
        let max_iterations = 5;

        for _i in 0..max_iterations {
            let user_prompt = format!(
                "## Tarefa:\n{}\n\n## Histórico de Ferramentas:\n{}\n\nExecute a tarefa. Quando concluir, retorne apenas o resultado final em linguagem natural.",
                user_message,
                if tool_history.is_empty() { "(nenhuma ferramenta usada ainda)".to_string() } else { tool_history.clone() }
            );

            let req = ChatRequest {
                messages: Vec::new(),
                model: model.to_string(),
                system: system_prompt.clone(),
                user: user_prompt,
                tools: Some(descriptors.clone()),
            };

            let resp = provider
                .chat(req)
                .context("LLM falhou ao processar a task")?;

            // Se há tool_calls, executa
            if let Some(ref calls) = resp.tool_calls {
                progress.push("✍️ Executando ações...".to_string());

                for call in calls {
                    let args = serde_json::from_str(&call.function.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));

                    // Policy check
                    let policy = ToolPolicyPipeline::new(PermissionMode::FullAccess);
                    match policy.authorize(&call.function.name, &args) {
                        arreio_tools::ToolPolicy::Deny => {
                            tool_history.push_str(&format!(
                                "\n[Tool: {}] → NEGADA pela política de segurança",
                                call.function.name
                            ));
                            continue;
                        }
                        _ => {}
                    }

                    let result = registry.call(ToolRequest {
                        name: call.function.name.clone(),
                        arguments: args,
                    });

                    match result {
                        Ok(res) => {
                            tool_history.push_str(&format!(
                                "\n[Tool: {}] → {}\n{}",
                                call.function.name,
                                if res.success { "OK" } else { "ERRO" },
                                if res.success {
                                    res.output
                                } else {
                                    res.error.unwrap_or_default()
                                }
                            ));
                        }
                        Err(e) => {
                            tool_history.push_str(&format!(
                                "\n[Tool: {}] → ERRO: {}",
                                call.function.name, e
                            ));
                        }
                    }
                }
                continue;
            }

            // Sem tool_calls: retorna resultado
            progress.push("✅ Finalizando...".to_string());

            // Extrai arquivos criados do tool_history
            let files = Self::extract_files_from_history(&tool_history);

            // Validação básica: se criou arquivos .rs, tenta cargo check
            let (validation_cmd, validation_exit_code) = if files.iter().any(|f| f.ends_with(".rs"))
            {
                let cmd = "cargo check".to_string();
                let exit_code = std::process::Command::new("cargo")
                    .args(["check"])
                    .current_dir(&self.work_dir)
                    .output()
                    .map(|o| {
                        if o.status.success() {
                            0
                        } else {
                            o.status.code().unwrap_or(-1)
                        }
                    })
                    .unwrap_or(-1);
                (Some(cmd), exit_code)
            } else {
                (None, 0)
            };

            // Monta mensagem final com lista de arquivos
            let mut message = resp.content;
            if !files.is_empty() {
                message.push_str("\n\n📁 Arquivos criados/modificados:\n");
                for f in &files {
                    message.push_str(&format!("  • {}\n", f));
                }
            }
            if validation_exit_code != 0 {
                message.push_str("\n⚠️ Validação falhou (cargo check). Verifique o código gerado.");
            }

            return Ok(TaskDispatchResult {
                success: true,
                message,
                files,
                validation_cmd,
                validation_exit_code,
            });
        }

        // Excedeu iterações
        Ok(TaskDispatchResult {
            success: false,
            message: "A tarefa é mais complexa do que eu posso resolver sozinho neste momento. \
Tente dividir em partes menores ou use 'arreio run <arquivo>' para uma execução completa."
                .to_string(),
            files: vec![],
            validation_cmd: None,
            validation_exit_code: -1,
        })
    }

    /// Gera mensagem de progresso a partir dos steps.
    pub fn format_progress(&self, progress: &[String]) -> String {
        progress.join("\n")
    }

    /// Extrai caminhos de arquivos criados/modificados do histórico de tools.
    fn extract_files_from_history(tool_history: &str) -> Vec<String> {
        let mut files = Vec::new();
        for line in tool_history.lines() {
            // Procura por padrões como: [Tool: write_file] → OK {path: "..."}
            if line.contains("write_file") || line.contains("edit_file") {
                // Extrai caminhos entre aspas
                for part in line.split('"') {
                    if part.contains('.') && !part.contains(' ') && part.len() > 2 {
                        // Heurística: parece um caminho de arquivo
                        if !files.contains(&part.to_string()) {
                            files.push(part.to_string());
                        }
                    }
                }
            }
        }
        files
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn build_tool_registry(&self) -> Result<ToolRegistry> {
        let registry = ToolRegistry::new();
        let descriptors = arreio_tools::build_native_tool_descriptors();

        for desc in descriptors {
            let name = desc.function.name.clone();
            let handler: std::sync::Arc<dyn arreio_tools::ToolHandler> = match name.as_str() {
                "read_file" => std::sync::Arc::new(arreio_tools::ReadFileHandler),
                "write_file" => std::sync::Arc::new(arreio_tools::WriteFileHandler {
                    safe_root: self.work_dir.clone(),
                }),
                "edit_file" => std::sync::Arc::new(arreio_tools::EditFileHandler {
                    safe_root: self.work_dir.clone(),
                }),
                "grep_search" => std::sync::Arc::new(arreio_tools::GrepSearchHandler),
                "glob_search" => std::sync::Arc::new(arreio_tools::GlobSearchHandler),
                "list_dir" => std::sync::Arc::new(arreio_tools::ListDirHandler),
                "exec" => {
                    let timeout_secs = std::env::var("ARREIO_EXEC_TIMEOUT_SECS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(30);
                    std::sync::Arc::new(arreio_tools::ExecHandler {
                        safe_root: self.work_dir.clone(),
                        timeout_secs,
                    })
                }
                "memory_search" => std::sync::Arc::new(arreio_tools::MemorySearchHandler {
                    blackboard: self.blackboard.clone(),
                }),
                "memory_write" => std::sync::Arc::new(arreio_tools::MemoryWriteHandler {
                    blackboard: self.blackboard.clone(),
                }),
                "checkpoint_save" => std::sync::Arc::new(arreio_tools::CheckpointSaveHandler),
                "checkpoint_rollback" => std::sync::Arc::new(arreio_tools::CheckpointRollbackHandler),
                "web_search" => std::sync::Arc::new(arreio_tools::WebSearchHandler),
                "web_fetch" => std::sync::Arc::new(arreio_tools::WebFetchHandler),
                "describe_image" => std::sync::Arc::new(arreio_tools::DescribeImageHandler),
                "synthesize_speech" => std::sync::Arc::new(arreio_tools::SynthesizeSpeechHandler),
                "transcribe_audio" => std::sync::Arc::new(arreio_tools::TranscribeAudioHandler),
                _ => continue,
            };
            registry.register(desc, handler);
        }

        Ok(registry)
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_result_sucesso() {
        let result = TaskDispatchResult {
            success: true,
            message: "Criei o arquivo!".to_string(),
            files: vec!["hello.rs".to_string()],
            validation_cmd: None,
            validation_exit_code: 0,
        };
        assert!(result.success);
        assert_eq!(result.files.len(), 1);
    }

    #[test]
    fn format_progress_une_linhas() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let p = tmp.path().to_path_buf();
        drop(tmp);
        let bb = Blackboard::open(&p).unwrap();
        let dispatcher = A2ATaskDispatcher::new(bb);
        let progress = vec!["⏳ Analisando...".to_string(), "✅ Pronto!".to_string()];
        let formatted = dispatcher.format_progress(&progress);
        assert!(formatted.contains("Analisando"));
        assert!(formatted.contains("Pronto"));
    }

    #[test]
    fn extrai_arquivos_do_history() {
        let history = r#"
[Tool: write_file] → OK {"path": "src/main.rs", "content": "fn main() {}"}
[Tool: read_file] → OK src/main.rs
[Tool: edit_file] → OK {"path": "Cargo.toml"}
"#;
        let files = A2ATaskDispatcher::extract_files_from_history(history);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"Cargo.toml".to_string()));
    }
}
