//! Client Registry — Camada 1 de Autenticação (OpenClaw ADR-003).
//!
//! Registro de clientes autenticados por API key. Cada cliente possui
//! um identificador único e uma chave cujo hash SHA-256 é armazenado
//! no Blackboard — o token em claro nunca é persistido.
//!
//! ## Fluxo
//! 1. `register_client()` → gera API key, grava hash, retorna key (única vez)
//! 2. `authenticate_client()` → recebe key raw, faz hash, busca no Blackboard
//! 3. `revoke_client()` → remove registro do Blackboard
//! 4. `list_clients()` → lista todos os clientes registrados
//!
//! ## Segurança
//! - Chaves armazenadas como SHA-256 (determinístico, para lookup por hash)
//! - Token raw nunca é logado ou persistido
//! - Blackboard provê isolamento entre sessões

use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use arreio_security::hash_token;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Estruturas de Dados ───────────────────────────────────────────────────────

/// Registro de um cliente no sistema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRecord {
    /// Identificador único do cliente.
    pub client_id: String,
    /// Hash SHA-256 da API key (nunca o token em claro).
    pub key_hash: String,
    /// Role atribuída (admin, developer, auditor, guest).
    pub role: String,
    /// Descrição legível (ex: "Servidor de CI", "Dashboard").
    pub description: String,
    /// Timestamp de criação (Unix epoch).
    pub created_at: u64,
}

/// Resultado da autenticação de um cliente.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub client_id: String,
    pub role: String,
}

// ── ClientRegistry ────────────────────────────────────────────────────────────

/// Registro de clientes persistido no Blackboard (categoria "clients").
pub struct ClientRegistry {
    blackboard: Blackboard,
}

impl ClientRegistry {
    /// Cria um novo registry vinculado ao Blackboard.
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    // ── Registro ──────────────────────────────────────────────────────────

    /// Registra um novo cliente e retorna a API key em claro.
    ///
    /// A API key é exibida **uma única vez**. Após esta chamada,
    /// apenas o hash SHA-256 é armazenado.
    ///
    /// # Erros
    /// - Se `client_id` já existe no registry.
    pub fn register(
        &self,
        client_id: &str,
        role: &str,
        description: &str,
    ) -> Result<String> {
        // Verifica se já existe
        if self.get(client_id)?.is_some() {
            bail!("cliente '{}' já existe", client_id);
        }

        let raw_key = format!("arreio_{}", Uuid::new_v4().to_string().replace('-', ""));
        let key_hash = hash_token(&raw_key);
        let now = now_epoch();

        let record = ClientRecord {
            client_id: client_id.to_string(),
            key_hash,
            role: role.to_string(),
            description: description.to_string(),
            created_at: now,
        };

        let value = serde_json::to_value(&record)?;
        self.blackboard
            .put_tuple("clients", client_id, value)?;

        Ok(raw_key)
    }

    // ── Autenticação ──────────────────────────────────────────────────────

    /// Autentica um cliente a partir da API key em claro.
    ///
    /// Faz hash da key recebida e compara com o hash armazenado.
    /// Retorna `ClientIdentity` se autenticado, `None` se não encontrado.
    pub fn authenticate(&self, raw_key: &str) -> Result<Option<ClientIdentity>> {
        let key_hash = hash_token(raw_key);
        let clients = self.list_all()?;

        for record in &clients {
            if constant_time_eq(record.key_hash.as_bytes(), key_hash.as_bytes()) {
                return Ok(Some(ClientIdentity {
                    client_id: record.client_id.clone(),
                    role: record.role.clone(),
                }));
            }
        }

        Ok(None)
    }

    // ── Consulta ──────────────────────────────────────────────────────────

    /// Obtém um cliente por ID.
    pub fn get(&self, client_id: &str) -> Result<Option<ClientRecord>> {
        match self.blackboard.get_tuple("clients", client_id) {
            Some(value) => {
                let record: ClientRecord = serde_json::from_value(value)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// Lista todos os clientes registrados.
    pub fn list_all(&self) -> Result<Vec<ClientRecord>> {
        let tuples = self.blackboard.search_tuples("clients", "");
        let mut records: Vec<ClientRecord> = Vec::new();
        for (_key, value) in tuples {
            if let Ok(record) = serde_json::from_value::<ClientRecord>(value) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.client_id.cmp(&b.client_id));
        Ok(records)
    }

    // ── Revogação ─────────────────────────────────────────────────────────

    /// Revoga (remove) um cliente do registry.
    ///
    /// # Erros
    /// - Se `client_id` não existe.
    pub fn revoke(&self, client_id: &str) -> Result<()> {
        if self.get(client_id)?.is_none() {
            bail!("cliente '{}' não encontrado", client_id);
        }
        self.blackboard.delete_tuple("clients", client_id)?;
        Ok(())
    }

    /// Conta o número de clientes registrados.
    pub fn count(&self) -> Result<usize> {
        Ok(self.list_all()?.len())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Comparação timing-safe para hashes de chaves.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for i in 0..a.len() {
        result |= a[i] ^ b[i];
    }
    result == 0
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn new_registry() -> ClientRegistry {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&path).unwrap();
        ClientRegistry::new(bb)
    }

    #[test]
    fn register_e_authenticate() {
        let reg = new_registry();
        let key = reg.register("agent-1", "developer", "Agente de build").unwrap();
        assert!(!key.is_empty());
        assert!(key.starts_with("arreio_"));

        let identity = reg.authenticate(&key).unwrap();
        assert!(identity.is_some());
        let id = identity.unwrap();
        assert_eq!(id.client_id, "agent-1");
        assert_eq!(id.role, "developer");
    }

    #[test]
    fn authenticate_key_errada() {
        let reg = new_registry();
        let key = reg.register("agent-1", "developer", "").unwrap();
        // Usa chave diferente
        let wrong_key = key.clone() + "x";
        let identity = reg.authenticate(&wrong_key).unwrap();
        assert!(identity.is_none());
    }

    #[test]
    fn authenticate_cliente_inexistente() {
        let reg = new_registry();
        let identity = reg.authenticate("arreio_fakekey123").unwrap();
        assert!(identity.is_none());
    }

    #[test]
    fn registro_duplicado_falha() {
        let reg = new_registry();
        reg.register("dup", "guest", "").unwrap();
        let result = reg.register("dup", "admin", "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("já existe"));
    }

    #[test]
    fn revoke_remove_cliente() {
        let reg = new_registry();
        let key = reg.register("temp", "guest", "").unwrap();
        assert_eq!(reg.count().unwrap(), 1);

        reg.revoke("temp").unwrap();
        assert_eq!(reg.count().unwrap(), 0);

        // Chave revogada não autentica mais
        let identity = reg.authenticate(&key).unwrap();
        assert!(identity.is_none());
    }

    #[test]
    fn revoke_inexistente_falha() {
        let reg = new_registry();
        let result = reg.revoke("fantasma");
        assert!(result.is_err());
    }

    #[test]
    fn list_all_ordenado() {
        let reg = new_registry();
        reg.register("zeta", "guest", "").unwrap();
        reg.register("alpha", "admin", "").unwrap();
        reg.register("beta", "developer", "").unwrap();

        let clients = reg.list_all().unwrap();
        let ids: Vec<&str> = clients.iter().map(|c| c.client_id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta", "zeta"]);
    }

    #[test]
    fn get_existente() {
        let reg = new_registry();
        reg.register("agent-x", "auditor", "Agente X").unwrap();
        let record = reg.get("agent-x").unwrap().unwrap();
        assert_eq!(record.client_id, "agent-x");
        assert_eq!(record.role, "auditor");
        assert_eq!(record.description, "Agente X");
    }

    #[test]
    fn get_inexistente() {
        let reg = new_registry();
        assert!(reg.get("fantasma").unwrap().is_none());
    }

    #[test]
    fn multiplos_clientes() {
        let reg = new_registry();
        let k1 = reg.register("a", "admin", "").unwrap();
        let k2 = reg.register("b", "developer", "").unwrap();

        let id1 = reg.authenticate(&k1).unwrap().unwrap();
        let id2 = reg.authenticate(&k2).unwrap().unwrap();

        assert_eq!(id1.client_id, "a");
        assert_eq!(id1.role, "admin");
        assert_eq!(id2.client_id, "b");
        assert_eq!(id2.role, "developer");
        assert_eq!(reg.count().unwrap(), 2);
    }

    #[test]
    fn constant_time_eq_mesmo_tamanho() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn constant_time_eq_tamanho_diferente() {
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
