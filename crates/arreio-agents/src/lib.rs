//! arreio-agents — Multi-Agent Routing, Registry e Subagent Spawning.
//!
//! Traduz o padrão "Multi-Agent Routing" do OpenClaw para a arquitetura
//! DAG + Blackboard do Arreio.

pub mod a2a_dispatcher;
pub mod delegate;

use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

pub use a2a_dispatcher::{A2ATaskDispatcher, TaskDispatchResult};
pub use delegate::{DelegateManager, DelegateProgress, DelegateResult, DelegateTask};

// ── Agente ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub agent_id: String,
    pub name: String,
    pub role: AgentRole,
    pub workspace_dir: Option<String>,
    pub provider: String,
    pub model: String,
    pub tool_allowlist: Vec<String>,
    pub permission_mode: String,
    pub channel_bindings: Vec<String>,
    pub max_spawn_depth: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRole {
    Architect,
    Developer,
    Tester,
    DevOps,
    Security,
    General,
}

impl AgentRole {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "architect" => Some(Self::Architect),
            "developer" => Some(Self::Developer),
            "tester" => Some(Self::Tester),
            "devops" => Some(Self::DevOps),
            "security" => Some(Self::Security),
            "general" => Some(Self::General),
            _ => None,
        }
    }
}

// ── Agent Registry ────────────────────────────────────────────────────────────

/// Registro de agentes persistido no Blackboard.
pub struct AgentRegistry {
    blackboard: Blackboard,
}

impl AgentRegistry {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn register(&self, config: &AgentConfig) -> Result<()> {
        let key = format!("{}", config.agent_id);
        let value = serde_json::to_value(config)?;
        self.blackboard.put_tuple("agents", &key, value)?;
        Ok(())
    }

    pub fn get(&self, agent_id: &str) -> Result<Option<AgentConfig>> {
        let key = format!("{}", agent_id);
        match self.blackboard.get_tuple("agents", &key) {
            Some(v) => Ok(Some(serde_json::from_value(v)?)),
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<AgentConfig>> {
        let tuples = self.blackboard.search_tuples("agents", "");
        let mut agents = Vec::new();
        for (_, value) in tuples {
            if let Ok(agent) = serde_json::from_value::<AgentConfig>(value) {
                agents.push(agent);
            }
        }
        Ok(agents)
    }

    pub fn remove(&self, agent_id: &str) -> Result<()> {
        // Blackboard não tem delete, então sobrescreve com null
        self.blackboard
            .put_tuple("agents", agent_id, serde_json::Value::Null)?;
        Ok(())
    }
}

// ── Message Router ────────────────────────────────────────────────────────────

/// Roteia inputs para o agente correto baseado em regras declarativas.
pub struct MessageRouter {
    blackboard: Blackboard,
}

impl MessageRouter {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Resolve qual agente deve processar uma mensagem.
    pub fn resolve(&self, content: &str, channel: Option<&str>) -> Result<Option<String>> {
        let agents = self.list_agents()?;

        // 1. Channel binding tem prioridade máxima
        if let Some(ch) = channel {
            for agent in &agents {
                if agent.channel_bindings.iter().any(|b| b == ch) {
                    return Ok(Some(agent.agent_id.clone()));
                }
            }
        }

        // 2. Content routing por keywords/regex
        for agent in &agents {
            if let Some(matched) = self.match_by_content(content, agent) {
                return Ok(Some(matched));
            }
        }

        // 3. Fallback: agente "default" ou primeiro da lista
        if let Some(default) = agents.iter().find(|a| a.agent_id == "default") {
            return Ok(Some(default.agent_id.clone()));
        }
        if let Some(first) = agents.first() {
            return Ok(Some(first.agent_id.clone()));
        }

        Ok(None)
    }

    fn list_agents(&self) -> Result<Vec<AgentConfig>> {
        let tuples = self.blackboard.search_tuples("agents", "");
        let mut agents = Vec::new();
        for (_, value) in tuples {
            if value.is_null() {
                continue;
            }
            if let Ok(agent) = serde_json::from_value::<AgentConfig>(value) {
                agents.push(agent);
            }
        }
        Ok(agents)
    }

    fn match_by_content(&self, content: &str, agent: &AgentConfig) -> Option<String> {
        // Keywords por role
        let keywords: Vec<&str> = match agent.role {
            AgentRole::Tester => vec!["test", "spec", "assert", "mock", "coverage"],
            AgentRole::DevOps => vec!["deploy", "docker", "k8s", "ci", "cd", "pipeline"],
            AgentRole::Security => vec!["audit", "secret", "vuln", "scan", "permission"],
            AgentRole::Architect => vec!["design", "schema", "diagram", "structure"],
            _ => vec![],
        };

        let content_lower = content.to_lowercase();
        if keywords.iter().any(|kw| content_lower.contains(kw)) {
            return Some(agent.agent_id.clone());
        }
        None
    }
}

// ── Subagent Spawner ──────────────────────────────────────────────────────────

/// Spawna subagentes como nós DAG filhos.
pub struct SubagentSpawner {
    blackboard: Blackboard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub parent_agent_id: String,
    pub task_spec: String,
    pub context_mode: ContextMode,
    pub assigned_agent_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContextMode {
    Fork,    // Copia contexto do pai
    Isolate, // Contexto limpo
}

impl SubagentSpawner {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Verifica se o spawn é permitido (limite de profundidade).
    pub fn can_spawn(&self, parent_agent_id: &str) -> Result<bool> {
        let registry = AgentRegistry::new(self.blackboard.clone());
        if let Some(parent) = registry.get(parent_agent_id)? {
            let current_depth = self.get_spawn_depth(parent_agent_id)?;
            Ok(current_depth < parent.max_spawn_depth)
        } else {
            Ok(false)
        }
    }

    /// Registra um spawn request no Blackboard para o DAG executor consumir.
    pub fn spawn(&self, request: SpawnRequest) -> Result<String> {
        let spawn_id = format!("spawn-{}", uuid::Uuid::new_v4());
        let value = serde_json::to_value(request)?;
        self.blackboard.put_tuple("spawns", &spawn_id, value)?;
        Ok(spawn_id)
    }

    /// Lista spawn requests pendentes.
    pub fn list_pending(&self) -> Result<Vec<(String, SpawnRequest)>> {
        let tuples = self.blackboard.search_tuples("spawns", "");
        let mut result = Vec::new();
        for (key, value) in tuples {
            if value.is_null() {
                continue;
            }
            if let Ok(req) = serde_json::from_value::<SpawnRequest>(value) {
                result.push((key, req));
            }
        }
        Ok(result)
    }

    /// Marca spawn como concluído.
    pub fn complete(&self, spawn_id: &str) -> Result<()> {
        self.blackboard
            .put_tuple("spawns", spawn_id, serde_json::Value::Null)?;
        Ok(())
    }

    fn get_spawn_depth(&self, agent_id: &str) -> Result<u32> {
        // Conta quantos spawns este agente já fez
        let spawns = self.list_pending()?;
        let count = spawns
            .iter()
            .filter(|(_, req)| req.parent_agent_id == agent_id)
            .count() as u32;
        Ok(count)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    #[test]
    fn registry_roundtrip() {
        let bb = make_bb();
        let reg = AgentRegistry::new(bb);
        let agent = AgentConfig {
            agent_id: "dev-1".into(),
            name: "Developer".into(),
            role: AgentRole::Developer,
            workspace_dir: Some("/tmp/dev".into()),
            provider: "ollama".into(),
            model: "qwen2.5-coder".into(),
            tool_allowlist: vec!["read_file".into(), "write_file".into()],
            permission_mode: "workspacewrite".into(),
            channel_bindings: vec!["cli".into()],
            max_spawn_depth: 3,
        };
        reg.register(&agent).unwrap();
        let retrieved = reg.get("dev-1").unwrap().unwrap();
        assert_eq!(retrieved.name, "Developer");
        assert_eq!(retrieved.role, AgentRole::Developer);
    }

    #[test]
    fn router_resolves_by_channel() {
        let bb = make_bb();
        let reg = AgentRegistry::new(bb.clone());
        let router = MessageRouter::new(bb);

        reg.register(&AgentConfig {
            agent_id: "slack-bot".into(),
            name: "Slack Bot".into(),
            role: AgentRole::General,
            workspace_dir: None,
            provider: "ollama".into(),
            model: "gemma4".into(),
            tool_allowlist: vec![],
            permission_mode: "readonly".into(),
            channel_bindings: vec!["slack".into()],
            max_spawn_depth: 2,
        })
        .unwrap();

        let resolved = router.resolve("hello", Some("slack")).unwrap();
        assert_eq!(resolved, Some("slack-bot".to_string()));
    }

    #[test]
    fn router_resolves_by_content() {
        let bb = make_bb();
        let reg = AgentRegistry::new(bb.clone());
        let router = MessageRouter::new(bb);

        reg.register(&AgentConfig {
            agent_id: "tester".into(),
            name: "Tester".into(),
            role: AgentRole::Tester,
            workspace_dir: None,
            provider: "ollama".into(),
            model: "gemma4".into(),
            tool_allowlist: vec![],
            permission_mode: "readonly".into(),
            channel_bindings: vec![],
            max_spawn_depth: 2,
        })
        .unwrap();

        let resolved = router.resolve("write tests for login", None).unwrap();
        assert_eq!(resolved, Some("tester".to_string()));
    }

    #[test]
    fn spawner_tracks_depth() {
        let bb = make_bb();
        let spawner = SubagentSpawner::new(bb.clone());
        let reg = AgentRegistry::new(bb);

        reg.register(&AgentConfig {
            agent_id: "parent".into(),
            name: "Parent".into(),
            role: AgentRole::Developer,
            workspace_dir: None,
            provider: "ollama".into(),
            model: "gemma4".into(),
            tool_allowlist: vec![],
            permission_mode: "workspacewrite".into(),
            channel_bindings: vec![],
            max_spawn_depth: 2,
        })
        .unwrap();

        assert!(spawner.can_spawn("parent").unwrap());
        spawner
            .spawn(SpawnRequest {
                parent_agent_id: "parent".into(),
                task_spec: "task 1".into(),
                context_mode: ContextMode::Isolate,
                assigned_agent_id: "child".into(),
            })
            .unwrap();
        assert!(spawner.can_spawn("parent").unwrap());
        spawner
            .spawn(SpawnRequest {
                parent_agent_id: "parent".into(),
                task_spec: "task 2".into(),
                context_mode: ContextMode::Isolate,
                assigned_agent_id: "child".into(),
            })
            .unwrap();
        // Depth = 2, max = 2, não pode mais
        assert!(!spawner.can_spawn("parent").unwrap());
    }
}
