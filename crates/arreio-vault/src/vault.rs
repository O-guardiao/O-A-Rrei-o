use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Entrada de secret no vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub name: String,
    pub value: String,
    pub created_at: u64,
    pub rotated_at: Option<u64>,
    pub exposed: bool,
    pub tags: Vec<String>,
}

/// Vault de secrets armazenado em JSON local.
/// Em produção enterprise, deve ser substituído por integração com KMS/HSM (HashiCorp Vault, AWS KMS, etc.).
pub struct SecretVault {
    path: PathBuf,
    secrets: HashMap<String, SecretEntry>,
}

impl SecretVault {
    pub fn open(path: &Path) -> Result<Self> {
        let secrets = if path.exists() {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("lendo vault em {}", path.display()))?;
            if raw.trim().is_empty() {
                HashMap::new()
            } else {
                serde_json::from_str(&raw)
                    .with_context(|| format!("vault corrompido em {}", path.display()))?
            }
        } else {
            HashMap::new()
        };
        Ok(Self {
            path: path.to_path_buf(),
            secrets,
        })
    }

    pub fn get(&self, name: &str) -> Option<&SecretEntry> {
        self.secrets.get(name)
    }

    pub fn set(&mut self, entry: SecretEntry) -> Result<()> {
        self.secrets.insert(entry.name.clone(), entry);
        self.persist()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.secrets.remove(name);
        self.persist()
    }

    /// Marca um secret como exposto (requer rotação).
    pub fn mark_exposed(&mut self, name: &str) -> Result<()> {
        if let Some(entry) = self.secrets.get_mut(name) {
            entry.exposed = true;
        }
        self.persist()
    }

    /// Rota um secret: gera novo valor aleatório (256 bits de entropia) e atualiza timestamp.
    pub fn rotate(&mut self, name: &str) -> Result<()> {
        if let Some(entry) = self.secrets.get_mut(name) {
            use rand::distributions::{Alphanumeric, DistString};
            entry.value = Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
            entry.rotated_at = Some(now());
            entry.exposed = false;
        }
        self.persist()
    }

    pub fn list(&self) -> Vec<&SecretEntry> {
        self.secrets.values().collect()
    }

    pub fn exposed_secrets(&self) -> Vec<&SecretEntry> {
        self.secrets.values().filter(|e| e.exposed).collect()
    }

    fn persist(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.secrets)?;
        fs::write(&self.path, raw)
            .with_context(|| format!("escrevendo vault em {}", self.path.display()))
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_vault() -> SecretVault {
        let f = NamedTempFile::new().unwrap();
        SecretVault::open(f.path()).unwrap()
    }

    #[test]
    fn vault_roundtrip() {
        let mut vault = temp_vault();
        vault
            .set(SecretEntry {
                name: "api_key".into(),
                value: "secret123".into(),
                created_at: 0,
                rotated_at: None,
                exposed: false,
                tags: vec![],
            })
            .unwrap();

        assert_eq!(vault.get("api_key").unwrap().value, "secret123");
    }

    #[test]
    fn vault_expose_and_rotate() {
        let mut vault = temp_vault();
        vault
            .set(SecretEntry {
                name: "db_pass".into(),
                value: "old".into(),
                created_at: 0,
                rotated_at: None,
                exposed: false,
                tags: vec![],
            })
            .unwrap();

        vault.mark_exposed("db_pass").unwrap();
        assert!(vault.get("db_pass").unwrap().exposed);

        vault.rotate("db_pass").unwrap();
        assert!(!vault.get("db_pass").unwrap().exposed);
        let rotated = &vault.get("db_pass").unwrap().value;
        assert_ne!(rotated, "old");
        assert_eq!(rotated.len(), 32);
    }
}
