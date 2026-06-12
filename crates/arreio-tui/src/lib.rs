//! arreio-tui — REPL interativo para O Arreio.
//!
//! Traduz o padrão "Interactive Mode" do OpenClaw para a arquitetura
//! stateless do Arreio.

use anyhow::Result;
use arreio_kernel::Blackboard;
use arreio_media::{ImageDescriber, SpeechSynthesizer};
use rustyline::DefaultEditor;
use std::path::Path;

/// REPL principal do Arreio.
pub struct ArreioRepl {
    blackboard: Blackboard,
    history_path: Option<String>,
}

impl ArreioRepl {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            history_path: None,
        }
    }

    pub fn with_history(mut self, path: impl Into<String>) -> Self {
        self.history_path = Some(path.into());
        self
    }

    /// Inicia o loop REPL.
    pub fn run<F>(&self, mut on_input: F) -> Result<()>
    where
        F: FnMut(&str) -> Result<String>,
    {
        let mut rl = DefaultEditor::new()?;

        if let Some(ref path) = self.history_path {
            if Path::new(path).exists() {
                let _ = rl.load_history(path);
            }
        }

        println!("╔══════════════════════════════════════════╗");
        println!("║     O Arreio REPL — Ctrl+C para sair      ║");
        println!("╚══════════════════════════════════════════╝");
        println!("Comandos: /plan, /run, /status, /skills, /memory, /rollback, /doctor, /quit");
        println!();

        loop {
            let readline = rl.readline("arreio> ");
            match readline {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(trimmed);

                    if trimmed == "/quit" || trimmed == "/q" {
                        println!("Saindo...");
                        break;
                    }

                    if let Some(response) = self.handle_slash_command(trimmed) {
                        println!("{}", response);
                        continue;
                    }

                    // Input normal: passa para o callback
                    match on_input(trimmed) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("[erro] {}", e),
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(rustyline::error::ReadlineError::Eof) => {
                    println!("^D");
                    break;
                }
                Err(e) => {
                    eprintln!("[repl] erro: {}", e);
                    break;
                }
            }
        }

        if let Some(ref path) = self.history_path {
            let _ = rl.save_history(path);
        }

        Ok(())
    }

    fn handle_slash_command(&self, cmd: &str) -> Option<String> {
        match cmd {
            "/help" | "/h" => Some(
                "Comandos disponíveis:\n\
                /plan <spec>     — Gera plano para spec\n\
                /run <spec>      — Executa pipeline\n\
                /status          — Mostra FSM + DAG\n\
                /skills          — Lista skills ativas\n\
                /memory <query>  — Busca na memória\n\
                /rollback        — Rollback para último checkpoint\n\
                /doctor          — Diagnóstico do sistema\n\
                /quit            — Sai do REPL"
                    .to_string(),
            ),
            "/status" => {
                let fsm = arreio_fsm::Fsm::new(self.blackboard.clone());
                let dag = arreio_dag::Dag::load(self.blackboard.clone()).ok()?;
                let s = dag.summary();
                Some(format!(
                    "FSM: {} | TODO:{} DOING:{} DONE:{} FAILED:{} TOTAL:{}",
                    fsm.current(),
                    s.todo,
                    s.doing,
                    s.done,
                    s.failed,
                    s.total
                ))
            }
            "/skills" => {
                let skills = self.blackboard.search_tuples("skills", "");
                if skills.is_empty() {
                    Some("Nenhuma skill ativa.".to_string())
                } else {
                    let mut out = vec!["Skills ativas:".to_string()];
                    for (k, _) in skills {
                        out.push(format!("  - {}", k));
                    }
                    Some(out.join("\n"))
                }
            }
            "/memory" => Some("Use /memory <query> para buscar.".to_string()),
            "/rollback" => Some("Rollback: execute 'arreio rollback' fora do REPL.".to_string()),
            "/doctor" => {
                let mut checks = vec!["Diagnóstico Arreio:".to_string()];
                checks.push(format!(
                    "  Blackboard: OK ({} tuplas)",
                    self.blackboard.search_tuples("", "").len()
                ));
                checks.push(format!(
                    "  FSM: {}",
                    arreio_fsm::Fsm::new(self.blackboard.clone()).current()
                ));
                Some(checks.join("\n"))
            }
            cmd if cmd.starts_with("/speak ") => {
                let text = cmd.strip_prefix("/speak ").unwrap_or("");
                if text.is_empty() {
                    Some("Uso: /speak <texto>".to_string())
                } else {
                    let tts = arreio_media::EspeakTts::new();
                    match tts.synthesize(text, None) {
                        Ok(result) => {
                            let path =
                                arreio_media::save_media(&result.audio_bytes, "repl_speech.wav")
                                    .ok()?;
                            Some(format!("Áudio sintetizado: {}", path.display()))
                        }
                        Err(e) => Some(format!("Erro TTS: {}", e)),
                    }
                }
            }
            cmd if cmd.starts_with("/describe ") => {
                let path = cmd.strip_prefix("/describe ").unwrap_or("");
                if path.is_empty() {
                    Some("Uso: /describe <caminho_da_imagem>".to_string())
                } else {
                    let describer = arreio_media::OllamaVisionDescriber::new("llava");
                    match describer.describe(std::path::Path::new(path), None) {
                        Ok(desc) => Some(desc),
                        Err(e) => Some(format!("Erro vision: {}", e)),
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn repl_criado() {
        let bb = temp_bb();
        let repl = ArreioRepl::new(bb);
        assert_eq!(repl.history_path, None);
    }
}
