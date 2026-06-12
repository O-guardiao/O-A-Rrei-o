//! Chat Transparente — interface unificada para usuários leigos.
//!
//! Integra todos os componentes de transparência:
//!   • TransparentSessionManager — sessões automáticas
//!   • IntentClassifier — detecta task vs chat
//!   • AutoCompressor — compressão automática
//!   • AutoLifecycle — pausa, resume, despedida
//!   • A2ATaskDispatcher — executa tasks internamente
//!
//! O usuário simplesmente digita e conversa. Zero comandos obrigatórios.

use anyhow::Result;
use arreio_agents::A2ATaskDispatcher;
use arreio_kernel::Blackboard;
use arreio_memory::{
    AutoCompressor, AutoLifecycle, ChatConsolidator, ChatRole, HelpContextual, IntentClassifier,
    OnboardingWizard, TransparentSessionManager, UserIntent,
};
use arreio_provider::ProviderClient;
use std::io::{stdin, stdout, Write};

/// Executa o chat transparente (modo interativo).
///
/// Chamado quando o usuário executa `arreio` sem argumentos.
pub fn run_transparent_chat(bb: Blackboard, model: &str) -> Result<()> {
    let session_mgr = TransparentSessionManager::new(bb.clone());
    let classifier = IntentClassifier::new();
    let mut compressor = AutoCompressor::new(bb.clone());
    let lifecycle = AutoLifecycle::new(bb.clone());
    let dispatcher = A2ATaskDispatcher::new(bb.clone());
    // Honra o prefixo `provider:modelo` do --model (ex.: anthropic:claude-...,
    // kimi:kimi-k2.5). Sem prefixo, mantém o padrão histórico (Ollama local).
    let provider: Box<dyn ProviderClient> = crate::build_single_provider(model, &bb)?;
    // Nome do modelo sem o prefixo de provider, para enviar à API.
    let model = model.split_once(':').map(|(_, m)| m).unwrap_or(model);
    let help = HelpContextual::new(bb.clone());
    let consolidator = ChatConsolidator::new(bb.clone());

    // ── Onboarding (primeira execução) ───────────────────────────────────
    let wizard = OnboardingWizard::new(bb.clone());
    let user_profile = if !wizard.is_complete()? {
        let profile = wizard.run(|question| {
            println!("{}", question);
            print!("> ");
            stdout().flush()?;
            let mut input = String::new();
            stdin().read_line(&mut input)?;
            Ok(input.trim().into())
        })?;
        println!("[arreio] Perfeito! Vou lembrar de tudo. Pode começar!");
        println!("[arreio] {}\n", wizard.profile_context());
        Some(profile)
    } else {
        wizard.load_profile().ok()
    };

    // Obtém ou cria sessão
    let active = session_mgr.get_or_create("cli", model)?;

    if active.is_new {
        println!("[arreio] Olá! Como posso ajudar você hoje?");
    } else {
        let welcome = lifecycle.welcome_back_message(active.session.title.as_deref());
        println!("[arreio] {}", welcome);
    }

    // Define título se for nova sessão
    if active.is_new {
        // Título será definido após primeira mensagem do usuário
    }

    let mut session_id = active.session.id;
    let stdin = stdin();
    let mut stdout = stdout();

    loop {
        print!("\n> ");
        stdout.flush()?;
        let mut input = String::new();
        stdin.read_line(&mut input)?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Comandos power user (opcionais, documentados em /help)
        if trimmed == "/quit" || trimmed == "/sair" {
            println!("[arreio] {}", lifecycle.farewell_message());
            break;
        }

        if trimmed == "/help" || trimmed == "/ajuda" {
            let help_text = help.help_message(Some(trimmed));
            if help_text.trim().is_empty()
                || help_text == "💡 Você pode:\n\nComandos opcionais: /help, /info, /new, /quit\n"
            {
                print_help();
            } else {
                println!("{}", help_text);
            }
            continue;
        }

        if trimmed == "/info" {
            show_session_info(&session_mgr, &session_id)?;
            continue;
        }

        if trimmed == "/new" || trimmed == "/nova" {
            let new_active = session_mgr.get_or_create("cli", model)?;
            session_id = new_active.session.id;
            println!("[arreio] Nova conversa iniciada.");
            continue;
        }

        // Verifica se precisa de fork automático (budget exaurido)
        if let Some(forked) = session_mgr.auto_fork_if_needed(&session_id)? {
            session_id = forked.id;
            println!("[arreio] Conversa continuada em nova sessão (contexto resumido).");
        }

        // Atualiza atividade
        session_mgr.touch(&session_id)?;

        // Define título na primeira mensagem
        session_mgr.set_title_if_empty(&session_id, &session_mgr.auto_title(trimmed))?;

        // Adiciona mensagem do usuário
        let user_tokens = trimmed.len() / 4;
        session_mgr.append_message(
            &session_id,
            ChatRole::User,
            trimmed,
            None,
            None,
            user_tokens,
        )?;

        // ── Auto-compressão ────────────────────────────────────────────────
        let compress_result = compressor.check_and_compress(&session_id)?;
        if let Some(notification) = compress_result.notification {
            println!("[arreio] {}", notification);
        }

        // ── Classificação de intenção ──────────────────────────────────────
        let intent = classifier.classify(trimmed);

        let response = match intent.intent {
            UserIntent::Task => {
                println!("[arreio] ⏳ Vou fazer isso para você...");

                let dispatch_result = dispatcher.execute(trimmed, &provider, model)?;

                if dispatch_result.success {
                    println!("[arreio] ✅ {}", dispatch_result.message);
                } else {
                    println!("[arreio] ⚠️ {}", dispatch_result.message);
                }

                dispatch_result.message
            }

            UserIntent::Hybrid => {
                // Híbrido: responde conversacionalmente mas oferece executar
                let mut assembler = arreio_memory::ContextAssembler::new(bb.clone());
                let base_prompt = build_system_prompt(model, &user_profile, &wizard, &consolidator);
                let frame = assembler.assemble_fast(
                    &session_id,
                    &format!("{}\n\nVocê é um assistente amigável. O usuário fez uma pergunta que pode envolver uma ação. \
Responda de forma conversacional, mas ao final pergunte se quer que você execute algo.", base_prompt),
                )?;

                // Monta histórico multi-turn a partir das mensagens da sessão
                let mut messages: Vec<arreio_provider::ChatMessageRequest> = frame
                    .messages
                    .iter()
                    .map(|m| arreio_provider::ChatMessageRequest {
                        role: m.role.to_string(),
                        content: m.content.clone(),
                        reasoning_content: None,
                    })
                    .collect();
                // Adiciona mensagem atual do usuário
                messages.push(arreio_provider::ChatMessageRequest {
                    role: "user".to_string(),
                    content: trimmed.to_string(),
                    reasoning_content: None,
                });

                let req = arreio_provider::ChatRequest {
                    model: model.to_string(),
                    system: frame.system_prompt,
                    user: trimmed.to_string(),
                    messages,
                    tools: None,
                };

                match provider.chat(req) {
                    Ok(resp) => {
                        println!("\n{}", resp.content);
                        resp.content
                    }
                    Err(e) => {
                        let msg = format!("Desculpe, tive um problema: {}", e);
                        println!("[arreio] {}", msg);
                        msg
                    }
                }
            }

            UserIntent::Conversational => {
                // Monta contexto com histórico completo da sessão
                let mut assembler = arreio_memory::ContextAssembler::new(bb.clone());
                let base_prompt = build_system_prompt(model, &user_profile, &wizard, &consolidator);
                let frame = assembler.assemble_fast(
                    &session_id,
                    &format!(
                        "{}\n\nVocê é um assistente amigável e prestativo. Modelo: {}",
                        base_prompt, model
                    ),
                )?;

                // Monta histórico multi-turn a partir das mensagens da sessão
                let mut messages: Vec<arreio_provider::ChatMessageRequest> = frame
                    .messages
                    .iter()
                    .map(|m| arreio_provider::ChatMessageRequest {
                        role: m.role.to_string(),
                        content: m.content.clone(),
                        reasoning_content: None,
                    })
                    .collect();
                // Adiciona mensagem atual do usuário
                messages.push(arreio_provider::ChatMessageRequest {
                    role: "user".to_string(),
                    content: trimmed.to_string(),
                    reasoning_content: None,
                });

                let req = arreio_provider::ChatRequest {
                    model: model.to_string(),
                    system: frame.system_prompt,
                    user: trimmed.to_string(),
                    messages,
                    tools: None,
                };

                match provider.chat(req) {
                    Ok(resp) => {
                        println!("\n{}", resp.content);
                        resp.content
                    }
                    Err(e) => {
                        let msg = format!("Desculpe, tive um problema: {}", e);
                        println!("[arreio] {}", msg);
                        msg
                    }
                }
            }
        };

        // Persiste resposta do assistente
        let assistant_tokens = response.len() / 4;
        session_mgr.append_message(
            &session_id,
            ChatRole::Assistant,
            &response,
            None,
            None,
            assistant_tokens,
        )?;

        // ── Lifecycle: detecta loop ────────────────────────────────────────
        let msgs = session_mgr.list_messages(&session_id)?;
        if lifecycle.detect_loop(&msgs) {
            println!("[arreio] {}", lifecycle.loop_suggestion());
        }

        // ── Lifecycle: detecta fim de conversa ─────────────────────────────
        if lifecycle.detect_complete(&msgs) {
            println!("[arreio] {}", lifecycle.farewell_message());

            // Consolida sessão antes de sair
            let consolidation = consolidator.consolidate_session(&session_id)?;
            if consolidation.persisted > 0 {
                println!(
                    "[arreio] {} fatos salvos para memória futura.",
                    consolidation.persisted
                );
            }

            break;
        }

        // ── Sugestões contextuais ──────────────────────────────────────────
        if intent.intent != UserIntent::Task {
            let suggestions = help.suggest(trimmed);
            if !suggestions.is_empty() {
                println!("{}", help.format_suggestions(&suggestions));
            }
        }
    }

    // Consolida todas as sessões não-despedidas antes de sair
    let _ = consolidator.consolidate_all();

    Ok(())
}

/// Executa chat inline (uma mensagem e sai).
///
/// Chamado quando o usuário executa `arreio "mensagem"`.
pub fn run_inline_chat(bb: Blackboard, model: &str, message: &str) -> Result<()> {
    let session_mgr = TransparentSessionManager::new(bb.clone());
    let classifier = IntentClassifier::new();
    // Honra o prefixo `provider:modelo` do --model; sem prefixo, Ollama local.
    let provider: Box<dyn ProviderClient> = crate::build_single_provider(model, &bb)?;
    let model = model.split_once(':').map(|(_, m)| m).unwrap_or(model);

    let active = session_mgr.get_or_create("cli", model)?;
    let session_id = active.session.id;

    // Define título
    session_mgr.set_title_if_empty(&session_id, &session_mgr.auto_title(message))?;

    // Persiste mensagem
    let inner_mgr = arreio_memory::SessionManager::new(bb.clone());
    inner_mgr.append_message(
        &session_id,
        ChatRole::User,
        message,
        None,
        None,
        message.len() / 4,
    )?;

    let intent = classifier.classify(message);

    let response = match intent.intent {
        UserIntent::Task => {
            let dispatcher = A2ATaskDispatcher::new(bb.clone());
            println!("[arreio] ⏳ Processando...");
            let result = dispatcher.execute(message, &provider, model)?;
            println!("[arreio] {}", result.message);
            result.message
        }
        _ => {
            let mut messages: Vec<arreio_provider::ChatMessageRequest> = Vec::new();
            messages.push(arreio_provider::ChatMessageRequest {
                role: "user".to_string(),
                content: message.to_string(),
                reasoning_content: None,
            });
            let req = arreio_provider::ChatRequest {
                model: model.to_string(),
                system: format!("Você é um assistente amigável. Modelo: {}", model),
                user: message.to_string(),
                messages,
                tools: None,
            };
            let resp = provider.chat(req)?;
            println!("{}", resp.content);
            resp.content
        }
    };

    inner_mgr.append_message(
        &session_id,
        ChatRole::Assistant,
        &response,
        None,
        None,
        response.len() / 4,
    )?;

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Monta o system prompt enriquecido com perfil e memórias consolidadas.
fn build_system_prompt(
    _model: &str,
    user_profile: &Option<arreio_memory::UserProfile>,
    wizard: &OnboardingWizard,
    consolidator: &ChatConsolidator,
) -> String {
    let mut parts = Vec::new();

    // Perfil do usuário (usando o perfil carregado em memória, não consultando ao Blackboard toda vez)
    if let Some(ref profile) = user_profile {
        let profile_text = format!(
            "Perfil do usuário: {} ({}). Objetivo: {}.",
            profile.business_name, profile.activity, profile.goal
        );
        parts.push(profile_text);
    } else {
        let profile = wizard.profile_context();
        if !profile.is_empty() {
            parts.push(profile);
        }
    }

    // Memórias consolidadas recentes
    if let Ok(all_facts) = consolidator.get_all_facts() {
        if !all_facts.is_empty() {
            let recent: Vec<String> = all_facts
                .iter()
                .rev()
                .take(5)
                .filter_map(|f| f.primary_text().map(|t| t.to_string()))
                .collect();
            if !recent.is_empty() {
                parts.push(format!(
                    "Contexto de conversas anteriores:\n{}",
                    recent
                        .iter()
                        .map(|f| format!("- {}", f))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
    }

    parts.join("\n\n")
}

fn print_help() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    ARREIO — Ajuda Rápida                       ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Basta digitar e conversar! O sistema cuida do resto.        ║");
    println!("║                                                              ║");
    println!("║  Comandos opcionais (power user):                            ║");
    println!("║    /help, /ajuda    — esta mensagem                          ║");
    println!("║    /info            — estatísticas da conversa               ║");
    println!("║    /new, /nova      — nova conversa                          ║");
    println!("║    /quit, /sair     — sair                                   ║");
    println!("║                                                              ║");
    println!("║  Para execução avançada:                                     ║");
    println!("║    arreio run <spec>  — pipeline SYMBION completo              ║");
    println!("║    arreio status      — estado do sistema                      ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}

fn show_session_info(session_mgr: &TransparentSessionManager, session_id: &str) -> Result<()> {
    let msgs = session_mgr.list_messages(session_id)?;
    println!(
        "[arreio] Sessão: {}...",
        &session_id[session_id.len().saturating_sub(8)..]
    );
    println!("  Mensagens: {}", msgs.len());
    Ok(())
}
