//! ChatConsolidator — extração automática de fatos de sessões conversacionais.
//!
//! Quando uma sessão é completada, extrai fatos duráveis usando heurísticas
//! determinísticas (regex) e persiste como MemoryEnvelope no Blackboard.
//!
//! Tipos de fatos extraídos:
//!   • Semantic — fatos sobre o usuário/negócio ("Eu tenho uma loja...")
//!   • Preference — preferências do usuário ("Prefiro planilhas...")
//!   • Decision — decisões tomadas ("Criei um arquivo...")
//!   • Error + Solution — problemas e resoluções
//!
//! Princípio: o sistema aprende com o usuário, não o contrário.

use anyhow::Result;
use arreio_kernel::Blackboard;
use regex::Regex;

use crate::envelope::{MemoryEnvelope, MemoryType, ModalityRef, Scope};
use crate::graph::{GraphStore, Relation};
use crate::session::{ChatMessage, ChatRole, SessionManager};

/// Resultado da consolidação de uma sessão.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationResult {
    pub session_id: String,
    pub extracted: usize,
    pub deduplicated: usize,
    pub persisted: usize,
    pub memory_ids: Vec<String>,
}

/// Consolidador automático de chats.
pub struct ChatConsolidator {
    session_mgr: SessionManager,
    graph: GraphStore,
    blackboard: Blackboard,
    max_facts_per_session: usize,
    max_total_facts: usize,
}

impl ChatConsolidator {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            session_mgr: SessionManager::new(blackboard.clone()),
            graph: GraphStore::new(blackboard.clone()),
            blackboard,
            max_facts_per_session: 100,
            max_total_facts: 500,
        }
    }

    /// Consolida uma sessão específica.
    pub fn consolidate_session(&self, session_id: &str) -> Result<ConsolidationResult> {
        let messages = self.session_mgr.list_messages(session_id)?;
        if messages.len() < 3 {
            return Ok(ConsolidationResult {
                session_id: session_id.into(),
                extracted: 0,
                deduplicated: 0,
                persisted: 0,
                memory_ids: vec![],
            });
        }

        let mut extracted = Vec::new();

        // 1. Extrai fatos declarativos do usuário
        extracted.extend(self.extract_facts(&messages, session_id));

        // 2. Extrai preferências
        extracted.extend(self.extract_preferences(&messages, session_id));

        // 3. Extrai decisões
        extracted.extend(self.extract_decisions(&messages, session_id));

        // 4. Extrai erros + soluções
        extracted.extend(self.extract_errors_and_solutions(&messages, session_id));

        // 5. Deduplica
        let deduplicated = self.deduplicate(&extracted);

        // 6. Aplica limite por sessão
        let limited: Vec<MemoryEnvelope> = deduplicated
            .into_iter()
            .take(self.max_facts_per_session)
            .collect();

        // 7. Persiste
        let mut persisted = 0;
        let mut memory_ids = Vec::new();

        for envelope in &limited {
            self.blackboard
                .put_tuple("memory", &envelope.id, serde_json::to_value(envelope)?)?;

            // Indexa no grafo
            let _ = self.graph.add_relation(&Relation {
                subject: session_id.into(),
                predicate: "derived_from".into(),
                object: envelope.id.clone(),
                confidence: envelope.confidence,
            });

            for tag in &envelope.tags {
                let _ = self.graph.add_relation(&Relation {
                    subject: envelope.id.clone(),
                    predicate: "tagged".into(),
                    object: tag.clone(),
                    confidence: envelope.importance,
                });
            }

            for entity in &envelope.entities {
                let _ = self.graph.add_relation(&Relation {
                    subject: envelope.id.clone(),
                    predicate: "mentions".into(),
                    object: entity.clone(),
                    confidence: envelope.confidence,
                });
            }

            persisted += 1;
            memory_ids.push(envelope.id.clone());
        }

        // 8. Marca sessão como consolidada
        self.blackboard.put_tuple(
            "memory",
            &format!("consolidated::{}", session_id),
            serde_json::to_value(&ConsolidationMarker {
                session_id: session_id.into(),
                memory_ids: memory_ids.clone(),
                consolidated_at: now_epoch_secs(),
            })?,
        )?;

        // 9. Aplica limite total de fatos
        self.enforce_total_limit()?;

        Ok(ConsolidationResult {
            session_id: session_id.into(),
            extracted: extracted.len(),
            deduplicated: limited.len() + (extracted.len() - limited.len()),
            persisted,
            memory_ids,
        })
    }

    /// Consolida todas as sessões não consolidadas.
    pub fn consolidate_all(&self) -> Result<Vec<ConsolidationResult>> {
        let sessions = self.session_mgr.list()?;
        let mut results = Vec::new();

        for session in sessions {
            // Verifica se já foi consolidada
            if self.is_consolidated(&session.id)? {
                continue;
            }

            // Consolida apenas sessões completadas ou velhas
            let is_complete = self.session_is_complete(&session);
            let is_old = self.session_is_old(&session);

            if is_complete || is_old {
                let result = self.consolidate_session(&session.id)?;
                if result.persisted > 0 {
                    results.push(result);
                }
            }
        }

        Ok(results)
    }

    /// Verifica se uma sessão já foi consolidada.
    pub fn is_consolidated(&self, session_id: &str) -> Result<bool> {
        let key = format!("consolidated::{}", session_id);
        Ok(self.blackboard.get_tuple("memory", &key).is_some())
    }

    /// Retorna fatos consolidados de uma sessão.
    pub fn get_facts_for_session(&self, session_id: &str) -> Result<Vec<MemoryEnvelope>> {
        let key = format!("consolidated::{}", session_id);
        let marker: ConsolidationMarker = match self.blackboard.get_tuple("memory", &key) {
            Some(v) => serde_json::from_value(v)?,
            None => return Ok(vec![]),
        };

        let mut facts = Vec::new();
        for mem_id in &marker.memory_ids {
            if let Some(v) = self.blackboard.get_tuple("memory", mem_id) {
                if let Ok(env) = serde_json::from_value::<MemoryEnvelope>(v) {
                    facts.push(env);
                }
            }
        }

        Ok(facts)
    }

    /// Retorna todos os fatos consolidados do usuário.
    pub fn get_all_facts(&self) -> Result<Vec<MemoryEnvelope>> {
        let all = self.blackboard.search_tuples("memory", "");
        let mut facts = Vec::new();

        for (key, value) in all {
            // Pula metadados (consolidated::, engram_)
            if key.starts_with("consolidated::") || key.starts_with("engram_") || key == "profile" {
                continue;
            }

            if let Ok(env) = serde_json::from_value::<MemoryEnvelope>(value) {
                facts.push(env);
            }
        }

        Ok(facts)
    }

    // ── Extração de fatos ────────────────────────────────────────────────────

    fn extract_facts(&self, messages: &[ChatMessage], session_id: &str) -> Vec<MemoryEnvelope> {
        let mut facts = Vec::new();

        let fact_patterns = [
            (
                r"(?i)(eu tenho|minha empresa|meu negócio|trabalho com|sou de|moro em|minha loja|meu escritório|meu comércio|minha fábrica)\s+(.{3,120})",
                MemoryType::Semantic,
                0.75,
            ),
            (
                r"(?i)(meu nome é|sou o|sou a|me chamo|chamo-me)\s+(.{2,60})",
                MemoryType::Semantic,
                0.8,
            ),
            (
                r"(?i)(uso|utilizo|minha ferramenta favorita|programa que uso)\s+(.{3,60})",
                MemoryType::Semantic,
                0.7,
            ),
            (
                r"(?i)(meu cliente|meus clientes|público alvo|público-alvo|clientela)\s+(.{3,120})",
                MemoryType::Semantic,
                0.75,
            ),
        ];

        for msg in messages.iter().filter(|m| m.role == ChatRole::User) {
            for (pattern, mem_type, importance) in &fact_patterns {
                if let Ok(re) = Regex::new(pattern) {
                    for cap in re.captures_iter(&msg.content) {
                        if let Some(matched) = cap.get(2) {
                            let text = matched.as_str().trim();
                            if text.len() >= 3 {
                                facts.push(self.build_envelope(
                                    session_id,
                                    text,
                                    mem_type.clone(),
                                    *importance,
                                    0.8,
                                    &["auto-extracted", "fact"],
                                ));
                            }
                        }
                    }
                }
            }
        }

        facts
    }

    fn extract_preferences(
        &self,
        messages: &[ChatMessage],
        session_id: &str,
    ) -> Vec<MemoryEnvelope> {
        let mut prefs = Vec::new();

        let pref_patterns = [
            (
                r"(?i)(prefiro|gosto de|quero que|não quero|odeio|não gosto|evito|sempre uso|nunca uso)\s+(.{3,120})",
                MemoryType::Preference,
                0.85,
            ),
            (
                r"(?i)(gostaria|queria|desejo|gostaria de|queria que)\s+(.{3,120})",
                MemoryType::Preference,
                0.75,
            ),
        ];

        for msg in messages.iter().filter(|m| m.role == ChatRole::User) {
            for (pattern, mem_type, importance) in &pref_patterns {
                if let Ok(re) = Regex::new(pattern) {
                    for cap in re.captures_iter(&msg.content) {
                        if let Some(matched) = cap.get(2) {
                            let text = matched.as_str().trim();
                            if text.len() >= 3 {
                                prefs.push(self.build_envelope(
                                    session_id,
                                    text,
                                    mem_type.clone(),
                                    *importance,
                                    0.85,
                                    &["auto-extracted", "preference"],
                                ));
                            }
                        }
                    }
                }
            }
        }

        prefs
    }

    fn extract_decisions(&self, messages: &[ChatMessage], session_id: &str) -> Vec<MemoryEnvelope> {
        let mut decisions = Vec::new();

        let decision_patterns = [
            (
                r"(?i)(criei|fiz|decidi|escolhi|implementei|configurei|organizei|montei|estabeleci)\s+(.{3,120})",
                MemoryType::Decision,
                0.65,
            ),
            (
                r"(?i)(vou usar|vou adotar|vou implementar|vou criar|vou fazer)\s+(.{3,120})",
                MemoryType::Decision,
                0.7,
            ),
        ];

        for msg in messages.iter().filter(|m| m.role == ChatRole::User) {
            for (pattern, mem_type, importance) in &decision_patterns {
                if let Ok(re) = Regex::new(pattern) {
                    for cap in re.captures_iter(&msg.content) {
                        if let Some(matched) = cap.get(2) {
                            let text = matched.as_str().trim();
                            if text.len() >= 3 {
                                decisions.push(self.build_envelope(
                                    session_id,
                                    text,
                                    mem_type.clone(),
                                    *importance,
                                    0.75,
                                    &["auto-extracted", "decision"],
                                ));
                            }
                        }
                    }
                }
            }
        }

        decisions
    }

    fn extract_errors_and_solutions(
        &self,
        messages: &[ChatMessage],
        session_id: &str,
    ) -> Vec<MemoryEnvelope> {
        let mut facts = Vec::new();

        // Detecta pares: erro (user) → solução (assistant)
        for window in messages.windows(2) {
            let user_msg = &window[0];
            let assistant_msg = &window[1];

            if user_msg.role != ChatRole::User || assistant_msg.role != ChatRole::Assistant {
                continue;
            }

            let is_error = Regex::new(r"(?i)(deu erro|não funcionou|falhou|bug|quebrou|crash|problema|deu problema|não deu certo|falhou ao)")
                .ok()
                .map(|re| re.is_match(&user_msg.content))
                .unwrap_or(false);

            let is_solution = Regex::new(
                r"(?i)(resolvi|solução|funcionou|corrigido|arrumado|correção|ajuste|fix|resolvido)",
            )
            .ok()
            .map(|re| re.is_match(&assistant_msg.content))
            .unwrap_or(false);

            if is_error && is_solution {
                let combined = format!(
                    "Erro: {} → Solução: {}",
                    user_msg.content.chars().take(80).collect::<String>(),
                    assistant_msg.content.chars().take(120).collect::<String>()
                );

                facts.push(self.build_envelope(
                    session_id,
                    &combined,
                    MemoryType::Solution,
                    0.9,
                    0.85,
                    &["auto-extracted", "error-solution"],
                ));
            }
        }

        facts
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn build_envelope(
        &self,
        session_id: &str,
        text: &str,
        memory_type: MemoryType,
        importance: f32,
        confidence: f32,
        extra_tags: &[&str],
    ) -> MemoryEnvelope {
        let id = format!("mem_{}", uuid::Uuid::new_v4());
        let mut tags: Vec<String> = extra_tags.iter().map(|t| t.to_string()).collect();
        tags.push(format!("{:?}", memory_type).to_lowercase());

        let entities = self.extract_entities(text);

        MemoryEnvelope {
            id,
            scope: Scope {
                tenant_id: None,
                user_id: Some("default".into()),
                agent_id: None,
                project_id: None,
                session_id: Some(session_id.into()),
            },
            memory_type,
            modalities: vec![ModalityRef {
                modality_type: "text".into(),
                content: text.into(),
            }],
            importance,
            confidence,
            entities,
            tags,
            content_hash: self.simple_hash(text),
            created_at: now_epoch_secs(),
        }
    }

    fn extract_entities(&self, text: &str) -> Vec<String> {
        let mut entities = Vec::new();

        // Nomes próprios (palavras capitalizadas consecutivas)
        if let Ok(re) = Regex::new(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)\b") {
            for cap in re.captures_iter(text) {
                if let Some(m) = cap.get(1) {
                    let word = m.as_str();
                    if word.len() > 2 && !self.is_common_word(word) {
                        entities.push(word.to_lowercase());
                    }
                }
            }
        }

        // Cidades/estados comuns
        let places = [
            "são paulo",
            "rio de janeiro",
            "belo horizonte",
            "curitiba",
            "porto alegre",
            "salvador",
            "recife",
            "fortaleza",
            "brasília",
            "goiânia",
            "manaus",
            "belém",
        ];
        let lower = text.to_lowercase();
        for place in &places {
            if lower.contains(place) {
                entities.push(place.to_string());
            }
        }

        entities.sort();
        entities.dedup();
        entities
    }

    fn is_common_word(&self, word: &str) -> bool {
        let common = [
            "Eu", "Você", "Ele", "Ela", "Nós", "Vós", "Eles", "Elas", "O", "A", "Os", "As", "Um",
            "Uma", "Uns", "Umas", "De", "Do", "Da", "Em", "No", "Na", "Para", "Por", "Com", "Sem",
            "Sobre", "Entre", "Mas", "E", "Ou", "Se", "Que", "Como", "Quando", "Onde", "Porque",
            "Isso", "Aquilo", "Este", "Esta", "Esse", "Essa", "Aquele", "Aquela",
        ];
        common.iter().any(|c| c.eq_ignore_ascii_case(word))
    }

    fn deduplicate(&self, envelopes: &[MemoryEnvelope]) -> Vec<MemoryEnvelope> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        for env in envelopes {
            let normalized = env.primary_text().unwrap_or("").to_lowercase();
            let key = self.simple_hash(&normalized);

            if !seen.contains(&key) {
                seen.insert(key);
                result.push(env.clone());
            }
        }

        result
    }

    fn enforce_total_limit(&self) -> Result<()> {
        let all = self.get_all_facts()?;
        if all.len() <= self.max_total_facts {
            return Ok(());
        }

        // Remove os mais antigos
        let mut sorted = all;
        sorted.sort_by_key(|e| e.created_at);
        let to_remove = sorted.len() - self.max_total_facts;

        for env in sorted.into_iter().take(to_remove) {
            self.blackboard.delete_tuple("memory", &env.id)?;
        }

        Ok(())
    }

    fn session_is_complete(&self, session: &crate::session::Session) -> bool {
        // Sessão suspensa e inativa há > 1 hora = considerada completa
        if !session.suspended {
            return false;
        }
        let inactive = now_epoch_secs().saturating_sub(session.updated_at);
        inactive > 3600 // 1 hora
    }

    fn session_is_old(&self, session: &crate::session::Session) -> bool {
        let age = now_epoch_secs().saturating_sub(session.created_at);
        age > 86400 // 24 horas
    }

    fn simple_hash(&self, input: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        input.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }
}

/// Marcador de consolidação persistido no Blackboard.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConsolidationMarker {
    pub session_id: String,
    pub memory_ids: Vec<String>,
    pub consolidated_at: u64,
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionMode;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_consolidator() -> ChatConsolidator {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        ChatConsolidator::new(bb)
    }

    #[test]
    fn extrai_fatos_declarativos() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Eu tenho uma loja de roupas masculinas em São Paulo",
                None,
                None,
                10,
            )
            .unwrap();
        c.session_mgr
            .append_message(&session.id, ChatRole::Assistant, "Legal!", None, None, 2)
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Trabalho com vendas online e presencial",
                None,
                None,
                8,
            )
            .unwrap();

        let result = c.consolidate_session(&session.id).unwrap();
        assert!(
            result.persisted >= 2,
            "deveria extrair pelo menos 2 fatos, extraiu {}",
            result.persisted
        );

        let facts = c.get_facts_for_session(&session.id).unwrap();
        assert!(!facts.is_empty());
    }

    #[test]
    fn extrai_preferencias() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Oi, preciso de ajuda",
                None,
                None,
                5,
            )
            .unwrap();
        c.session_mgr
            .append_message(&session.id, ChatRole::Assistant, "Claro!", None, None, 2)
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Prefiro planilhas em Excel ao invés de Google Sheets",
                None,
                None,
                10,
            )
            .unwrap();

        let result = c.consolidate_session(&session.id).unwrap();
        assert!(
            result.persisted >= 1,
            "deveria extrair preferência, persistiu {}",
            result.persisted
        );

        let facts = c.get_facts_for_session(&session.id).unwrap();
        assert!(facts
            .iter()
            .any(|f| f.memory_type == MemoryType::Preference));
    }

    #[test]
    fn deduplica_fatos_repetidos() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        c.session_mgr
            .append_message(&session.id, ChatRole::User, "Oi", None, None, 1)
            .unwrap();
        c.session_mgr
            .append_message(&session.id, ChatRole::Assistant, "Oi!", None, None, 1)
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Eu tenho uma loja em São Paulo",
                None,
                None,
                8,
            )
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Eu tenho uma loja em São Paulo",
                None,
                None,
                8,
            )
            .unwrap();

        let result = c.consolidate_session(&session.id).unwrap();
        assert_eq!(
            result.persisted, 1,
            "deveria deduplicar fatos repetidos, persistiu {}",
            result.persisted
        );
    }

    #[test]
    fn nao_consolida_sessao_curta() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        c.session_mgr
            .append_message(&session.id, ChatRole::User, "Oi", None, None, 1)
            .unwrap();

        let result = c.consolidate_session(&session.id).unwrap();
        assert_eq!(result.persisted, 0, "sessão curta não deve ser consolidada");
    }

    #[test]
    fn extrai_erro_e_solucao() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Oi, estou com problema",
                None,
                None,
                5,
            )
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::Assistant,
                "Qual o problema?",
                None,
                None,
                3,
            )
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Deu erro ao criar o arquivo",
                None,
                None,
                6,
            )
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::Assistant,
                "A solução é usar o comando correto: touch arquivo.txt",
                None,
                None,
                10,
            )
            .unwrap();

        let result = c.consolidate_session(&session.id).unwrap();
        assert!(
            result.persisted >= 1,
            "deveria extrair erro/solução, persistiu {}",
            result.persisted
        );

        let facts = c.get_facts_for_session(&session.id).unwrap();
        assert!(facts.iter().any(|f| f.memory_type == MemoryType::Solution));
    }

    #[test]
    fn is_consolidated_funciona() {
        let c = temp_consolidator();
        let session = c
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        assert!(!c.is_consolidated(&session.id).unwrap());

        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Eu tenho uma loja",
                None,
                None,
                5,
            )
            .unwrap();
        c.session_mgr
            .append_message(
                &session.id,
                ChatRole::User,
                "Trabalho com vendas",
                None,
                None,
                5,
            )
            .unwrap();
        c.session_mgr
            .append_message(&session.id, ChatRole::User, "Uso Excel", None, None, 3)
            .unwrap();

        c.consolidate_session(&session.id).unwrap();
        assert!(c.is_consolidated(&session.id).unwrap());
    }

    #[test]
    fn enforce_total_limit_funciona() {
        let c = temp_consolidator();

        // Cria muitas sessões com fatos
        for i in 0..10 {
            let session = c
                .session_mgr
                .create("cli", "gemma4", SessionMode::Conversational)
                .unwrap();
            for _ in 0..60 {
                c.session_mgr
                    .append_message(
                        &session.id,
                        ChatRole::User,
                        &format!("Fato número {} da sessão {}", i, i),
                        None,
                        None,
                        5,
                    )
                    .unwrap();
            }
            c.consolidate_session(&session.id).unwrap();
        }

        let all = c.get_all_facts().unwrap();
        assert!(
            all.len() <= 500,
            "deve respeitar limite total de 500, tem {}",
            all.len()
        );
    }
}
