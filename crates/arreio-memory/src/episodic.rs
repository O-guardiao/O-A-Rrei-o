use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Evento episódico — uma ação ocorrida em uma sessão.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EpisodicEvent {
    pub timestamp: u64,
    pub action: String,
    pub result: String,
    pub success: bool,
}

/// Memória episódica armazenada no Blackboard.
pub struct EpisodicMemory {
    blackboard: Blackboard,
}

impl EpisodicMemory {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Grava uma lista de eventos associados a uma sessão.
    pub fn record_session(&self, session_id: &str, events: Vec<EpisodicEvent>) -> Result<()> {
        let key = format!("{}:events", session_id);
        self.blackboard
            .put_tuple("memory:episodic", &key, serde_json::to_value(&events)?)
    }

    /// Recupera todos os eventos de uma sessão.
    pub fn recall_session(&self, session_id: &str) -> Result<Vec<EpisodicEvent>> {
        let key = format!("{}:events", session_id);
        match self.blackboard.get_tuple("memory:episodic", &key) {
            Some(v) => Ok(serde_json::from_value(v).unwrap_or_default()),
            None => Ok(Vec::new()),
        }
    }

    /// Busca eventos de todas as sessões cujo `action` ou `result` contenha a substring.
    pub fn search_similar(&self, objective: &str) -> Result<Vec<EpisodicEvent>> {
        let all = self.blackboard.search_tuples("memory:episodic", "");
        let objective_lower = objective.to_lowercase();
        let mut matches = Vec::new();
        for (_, v) in all {
            if let Ok(events) = serde_json::from_value::<Vec<EpisodicEvent>>(v) {
                for ev in events {
                    if ev.action.to_lowercase().contains(&objective_lower)
                        || ev.result.to_lowercase().contains(&objective_lower)
                    {
                        matches.push(ev);
                    }
                }
            }
        }
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_event(action: &str, result: &str, success: bool) -> EpisodicEvent {
        EpisodicEvent {
            timestamp: 1,
            action: action.into(),
            result: result.into(),
            success,
        }
    }

    #[test]
    fn episodic_memory_grava_e_recupera_sessao() {
        let bb = temp_bb();
        let mem = EpisodicMemory::new(bb);
        let events = vec![
            make_event("compilar", "sucesso", true),
            make_event("testar", "falha", false),
        ];
        mem.record_session("sess1", events.clone()).unwrap();
        let loaded = mem.recall_session("sess1").unwrap();
        assert_eq!(loaded, events);
    }

    #[test]
    fn episodic_memory_busca_similar_por_substring() {
        let bb = temp_bb();
        let mem = EpisodicMemory::new(bb);
        mem.record_session(
            "sess1",
            vec![
                make_event("build projeto A", "ok", true),
                make_event("deploy projeto A", "ok", true),
            ],
        )
        .unwrap();
        mem.record_session("sess2", vec![make_event("testar projeto B", "erro", false)])
            .unwrap();

        let results = mem.search_similar("projeto A").unwrap();
        assert_eq!(results.len(), 2);

        let results_err = mem.search_similar("erro").unwrap();
        assert_eq!(results_err.len(), 1);
    }

    #[test]
    fn episodic_memory_integracao_blackboard() {
        let bb = temp_bb();
        let mem = EpisodicMemory::new(bb.clone());
        mem.record_session("sess_bb", vec![make_event("ação", "resultado", true)])
            .unwrap();

        // Verifica que a tupla está no Blackboard com o prefixo correto
        let val = bb.get_tuple("memory:episodic", "sess_bb:events").unwrap();
        let arr: Vec<EpisodicEvent> = serde_json::from_value(val).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].action, "ação");
    }
}
