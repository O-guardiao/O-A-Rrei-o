use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

// ═══════════════════════════════════════════════════════════════════════════
// Código legado — AuditLog baseado em Blackboard
// ═══════════════════════════════════════════════════════════════════════════

/// Categoria de evento auditável.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditCategory {
    Auth,          // login, logout, token refresh
    Command,       // execução de shell
    FileWrite,     // escrita em disco
    FileRead,      // leitura de arquivo
    LlmCall,       // chamada a provedor LLM
    DagTransition, // mudança de estado no DAG
    FsmTransition, // mudança de estado na FSM
    SkillAction,   // criação, uso, remoção de skill
    Permission,    // aprovação/negativa HITL
    Config,        // alteração de configuração
}

/// Entrada imutável de audit trail usada pelo AuditLog (Blackboard).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    timestamp: u64,
    category: AuditCategory,
    actor: String,  // quem executou: user_id, agent_id, system
    action: String, // descrição curta
    target: String, // alvo da ação (arquivo, comando, etc.)
    details: serde_json::Value,
    session_id: String,
}

/// Logger de audit trail persistido no Blackboard (categoria "audit").
pub struct AuditLog {
    blackboard: Blackboard,
    session_id: String,
}

impl AuditLog {
    pub fn new(blackboard: Blackboard, session_id: impl Into<String>) -> Self {
        Self {
            blackboard,
            session_id: session_id.into(),
        }
    }

    pub fn log(
        &self,
        category: AuditCategory,
        actor: &str,
        action: &str,
        target: &str,
        details: serde_json::Value,
    ) -> Result<()> {
        let entry = LogEntry {
            timestamp: now(),
            category,
            actor: actor.into(),
            action: action.into(),
            target: target.into(),
            details,
            session_id: self.session_id.clone(),
        };
        let key = format!("{}-{}", entry.timestamp, uuid::Uuid::new_v4());
        let value = serde_json::to_value(entry)?;
        self.blackboard.put_tuple("audit", &key, value)
    }

    /// Recupera entradas de audit por categoria (últimas N).
    pub fn query(&self, category: Option<AuditCategory>, limit: usize) -> Vec<LogEntry> {
        let all = self.blackboard.search_tuples("audit", "");
        let mut entries: Vec<LogEntry> = all
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .filter(|e: &LogEntry| category.as_ref().map(|c| &e.category == c).unwrap_or(true))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        entries.truncate(limit);
        entries
    }

    /// Exporta audit trail como JSON Lines (formato SIEM-friendly).
    pub fn export_jsonl(&self) -> Result<String> {
        let all = self.blackboard.search_tuples("audit", "");
        let mut lines = Vec::new();
        for (_, v) in all {
            lines.push(serde_json::to_string(&v)?);
        }
        Ok(lines.join("\n"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// AuditTrail — append-only com integridade criptográfica encadeada
// ═══════════════════════════════════════════════════════════════════════════

/// Entrada individual do audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub timestamp: u64,
    pub action: String,
    pub actor: String,
    pub resource: String,
    pub details: String,
    pub prev_hash: String,
    pub hash: String,
}

/// Snapshot usado para serialização / desserialização do AuditTrail.
#[derive(Serialize, Deserialize)]
struct AuditTrailSnapshot {
    max_entries: usize,
    entries: Vec<AuditEntry>,
}

/// Audit trail imutável com hashes encadeados.
pub struct AuditTrail {
    entries: VecDeque<AuditEntry>,
    max_entries: usize,
}

impl AuditTrail {
    /// Cria um novo audit trail vazio com capacidade máxima definida.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
        }
    }

    /// Adiciona nova entrada. Calcula hash incluindo prev_hash.
    pub fn append(
        &mut self,
        action: &str,
        actor: &str,
        resource: &str,
        details: &str,
    ) -> Result<&AuditEntry> {
        let seq = self.entries.len() as u64 + 1;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let prev_hash = self
            .entries
            .back()
            .map(|e| e.hash.clone())
            .unwrap_or_default();

        let mut entry = AuditEntry {
            seq,
            timestamp,
            action: action.to_string(),
            actor: actor.to_string(),
            resource: resource.to_string(),
            details: details.to_string(),
            prev_hash,
            hash: String::new(),
        };
        entry.hash = compute_hash(&entry);

        // Rotação quando excede capacidade máxima.
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
        self.entries
            .back()
            .ok_or_else(|| anyhow::anyhow!("audit chain vazio imediatamente após inserção"))
    }

    /// Verifica integridade de todo o chain.
    /// Retorna Ok(()) se válido, ou erro na primeira quebra detectada.
    pub fn verify(&self) -> Result<()> {
        for (i, entry) in self.entries.iter().enumerate() {
            let expected = compute_hash(entry);
            if entry.hash != expected {
                anyhow::bail!(
                    "Integridade quebrada na entrada {} (seq={}): hash esperado {} mas encontrado {}",
                    i,
                    entry.seq,
                    expected,
                    entry.hash
                );
            }

            if i == 0 {
                if !entry.prev_hash.is_empty() {
                    anyhow::bail!(
                        "Primeira entrada (seq={}) deve ter prev_hash vazio",
                        entry.seq
                    );
                }
            } else {
                let prev = &self.entries[i - 1];
                if entry.prev_hash != prev.hash {
                    anyhow::bail!(
                        "Encadeamento quebrado na entrada {} (seq={}): prev_hash {} não corresponde ao hash anterior {}",
                        i,
                        entry.seq,
                        entry.prev_hash,
                        prev.hash
                    );
                }
            }
        }
        Ok(())
    }

    /// Exporta para JSON.
    pub fn to_json(&self) -> Result<String> {
        let snapshot = AuditTrailSnapshot {
            max_entries: self.max_entries,
            entries: self.entries.iter().cloned().collect(),
        };
        Ok(serde_json::to_string(&snapshot)?)
    }

    /// Importa de JSON.
    pub fn from_json(json: &str) -> Result<Self> {
        let snapshot: AuditTrailSnapshot = serde_json::from_str(json)?;
        let trail = Self {
            entries: snapshot.entries.into(),
            max_entries: snapshot.max_entries,
        };
        trail.verify()?;
        Ok(trail)
    }

    /// Busca entradas por actor.
    pub fn find_by_actor(&self, actor: &str) -> Vec<&AuditEntry> {
        self.entries.iter().filter(|e| e.actor == actor).collect()
    }

    /// Busca entradas por resource.
    pub fn find_by_resource(&self, resource: &str) -> Vec<&AuditEntry> {
        self.entries
            .iter()
            .filter(|e| e.resource == resource)
            .collect()
    }
}

/// Calcula SHA-256 sobre TODOS os campos estruturais + prev_hash da entrada.
/// `details` precisa entrar no hash: sem ele, o conteúdo da entrada poderia
/// ser adulterado sem quebrar `verify()`.
fn compute_hash(entry: &AuditEntry) -> String {
    let data = format!(
        "{}:{}:{}:{}:{}:{}:{}",
        entry.seq,
        entry.timestamp,
        entry.action,
        entry.actor,
        entry.resource,
        entry.details,
        entry.prev_hash
    );
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ═══════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn temp_log() -> AuditLog {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        AuditLog::new(bb, "test-session")
    }

    // ── Testes do AuditLog legado ──────────────────────────────────────────

    #[test]
    fn log_and_query() {
        let log = temp_log();
        log.log(
            AuditCategory::Command,
            "developer",
            "executed",
            "cargo test",
            serde_json::json!({"exit_code": 0}),
        )
        .unwrap();
        let entries = log.query(Some(AuditCategory::Command), 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "executed");
    }

    // ── Testes do AuditTrail ───────────────────────────────────────────────

    #[test]
    fn cria_trail_vazio() {
        let trail = AuditTrail::new(10);
        assert!(trail.entries.is_empty());
        trail.verify().unwrap(); // vazio é considerado válido
    }

    #[test]
    fn append_gera_hash_e_seq() {
        let mut trail = AuditTrail::new(10);
        let entry = trail.append("login", "alice", "app", "sucesso").unwrap();
        assert!(!entry.hash.is_empty());
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.prev_hash, "");
    }

    #[test]
    fn verify_detecta_adulteracao_de_details() {
        // Regressão: `details` precisa estar coberto pelo hash — antes deste
        // fix, adulterar `details` não quebrava verify().
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "sucesso").unwrap();
        trail.entries.back_mut().unwrap().details = "ADULTERADO".to_string();
        assert!(trail.verify().is_err());
    }

    #[test]
    fn chain_encadeado_corretamente() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "sucesso").unwrap();
        trail
            .append("read", "alice", "file.txt", "leitura ok")
            .unwrap();
        trail.append("logout", "alice", "app", "tchau").unwrap();

        assert_eq!(trail.entries.len(), 3);
        trail.verify().unwrap();

        let e1 = trail.entries.iter().nth(0).unwrap();
        let e2 = trail.entries.iter().nth(1).unwrap();
        let e3 = trail.entries.iter().nth(2).unwrap();

        assert_eq!(e2.prev_hash, e1.hash);
        assert_eq!(e3.prev_hash, e2.hash);
    }

    #[test]
    fn verify_detecta_hash_alterado() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "sucesso").unwrap();
        trail.entries.back_mut().unwrap().hash = "deadbeef".to_string();
        let result = trail.verify();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Integridade quebrada"));
    }

    #[test]
    fn verify_detecta_prev_hash_quebrado() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "sucesso").unwrap();
        trail
            .append("read", "alice", "file.txt", "leitura ok")
            .unwrap();
        // Altera o prev_hash e recalcula o hash para que a falha seja apenas no encadeamento.
        let last = trail.entries.back_mut().unwrap();
        last.prev_hash = "tampered".to_string();
        last.hash = compute_hash(last);
        let result = trail.verify();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Encadeamento quebrado"));
    }

    #[test]
    fn verify_detecta_prev_hash_nao_vazio_no_primeiro() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "sucesso").unwrap();
        trail.entries.front_mut().unwrap().prev_hash = "should_be_empty".to_string();
        // Recalcula o hash para não falhar no teste de integridade do hash em si
        let first = trail.entries.front_mut().unwrap();
        first.hash = compute_hash(first);
        let result = trail.verify();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("prev_hash vazio"));
    }

    #[test]
    fn max_entries_rotaciona() {
        let mut trail = AuditTrail::new(2);
        trail.append("a", "sys", "r1", "").unwrap();
        trail.append("b", "sys", "r2", "").unwrap();
        trail.append("c", "sys", "r3", "").unwrap();

        assert_eq!(trail.entries.len(), 2);
        let first = trail.entries.front().unwrap();
        assert_eq!(first.seq, 2); // a foi removido, b é o primeiro
        let last = trail.entries.back().unwrap();
        assert_eq!(last.seq, 3);
    }

    #[test]
    fn to_json_from_json_redondo() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "ok").unwrap();
        trail.append("write", "bob", "db", "update").unwrap();

        let json = trail.to_json().unwrap();
        let restored = AuditTrail::from_json(&json).unwrap();
        assert_eq!(restored.entries.len(), 2);
        restored.verify().unwrap();
        assert_eq!(restored.entries[0].actor, "alice");
        assert_eq!(restored.entries[1].actor, "bob");
    }

    #[test]
    fn from_json_detecta_tamper() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "ok").unwrap();
        let mut json = trail.to_json().unwrap();
        // Substitui o hash por lixo no JSON
        json = json.replace(&trail.entries[0].hash, "00000000000000000000000000000000");
        assert!(AuditTrail::from_json(&json).is_err());
    }

    #[test]
    fn find_by_actor() {
        let mut trail = AuditTrail::new(10);
        trail.append("login", "alice", "app", "").unwrap();
        trail.append("read", "bob", "file", "").unwrap();
        trail.append("write", "alice", "db", "").unwrap();

        let alice = trail.find_by_actor("alice");
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|e| e.actor == "alice"));
    }

    #[test]
    fn find_by_resource() {
        let mut trail = AuditTrail::new(10);
        trail.append("read", "alice", "file1", "").unwrap();
        trail.append("read", "bob", "file2", "").unwrap();
        trail.append("write", "alice", "file1", "").unwrap();

        let r1 = trail.find_by_resource("file1");
        assert_eq!(r1.len(), 2);
        assert!(r1.iter().all(|e| e.resource == "file1"));
    }

    #[test]
    fn primeira_entrada_prev_hash_vazio() {
        let mut trail = AuditTrail::new(10);
        trail.append("init", "system", "kernel", "boot").unwrap();
        let e = trail.entries.front().unwrap();
        assert_eq!(e.prev_hash, "");
        assert_eq!(e.seq, 1);
    }

    #[test]
    fn serializa_e_desserializa_max_entries() {
        let mut trail = AuditTrail::new(42);
        trail.append("x", "y", "z", "w").unwrap();
        let json = trail.to_json().unwrap();
        let restored = AuditTrail::from_json(&json).unwrap();
        assert_eq!(restored.max_entries, 42);
    }
}
