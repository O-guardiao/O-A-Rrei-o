//! AutoRotator — rotação automática de chaves com política e versionamento (PVC-Q3.2).
//!
//! Complementa o `KeyRotator` (que é apenas memória transitória) com:
//! - **persistência no Blackboard** (`vault::rotator:{provider}`) — o estado
//!   sobrevive a restarts;
//! - **política por provider** (`RotationPolicy`: intervalo + versões retidas);
//! - **versionamento**: a chave anterior é preservada (janela configurável)
//!   para permitir rollover gradual de consumidores;
//! - **auditoria**: cada rotação emite tupla `audit::vault_rotation:*`
//!   (sem expor o valor da chave — apenas metadados).
//!
//! Automação: um job do `arreio-scheduler` chama `rotate_due()` periodicamente.
//! O relógio é passado como parâmetro (`now_epoch`) para determinismo em teste.

use anyhow::{bail, Context, Result};
use arreio_kernel::Blackboard;
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};

/// Política de rotação por provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationPolicy {
    /// Idade máxima da chave antes da rotação obrigatória (dias).
    pub interval_days: u32,
    /// Quantas versões anteriores manter para rollover.
    pub keep_versions: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            interval_days: 30,
            keep_versions: 2,
        }
    }
}

/// Uma versão de chave (atual ou anterior).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVersion {
    pub value: String,
    pub version: u32,
    pub rotated_at: u64,
}

/// Estado persistido de um provider no rotator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotatorEntry {
    pub provider: String,
    pub policy: RotationPolicy,
    pub current: KeyVersion,
    /// Versões anteriores, mais recente primeiro (máx. `policy.keep_versions`).
    pub previous: Vec<KeyVersion>,
}

/// Evento de rotação (auditável; nunca contém o valor da chave).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationEvent {
    pub provider: String,
    pub old_version: u32,
    pub new_version: u32,
    pub rotated_at: u64,
}

/// Rotator automático persistido no Blackboard.
pub struct AutoRotator {
    blackboard: Blackboard,
}

impl AutoRotator {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    fn key_for(provider: &str) -> String {
        format!("rotator:{}", provider)
    }

    fn load(&self, provider: &str) -> Option<RotatorEntry> {
        self.blackboard
            .get_tuple("vault", &Self::key_for(provider))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    fn save(&self, entry: &RotatorEntry) -> Result<()> {
        self.blackboard
            .put_tuple(
                "vault",
                &Self::key_for(&entry.provider),
                serde_json::to_value(entry)?,
            )
            .context("persistindo estado do rotator")
    }

    /// Gera uma chave nova: 32 chars alfanuméricos via CSPRNG (~190 bits).
    fn generate_key() -> String {
        Alphanumeric.sample_string(&mut rand::thread_rng(), 32)
    }

    /// Registra um provider com a chave inicial e a política de rotação.
    pub fn register(
        &self,
        provider: &str,
        initial_key: &str,
        policy: RotationPolicy,
        now_epoch: u64,
    ) -> Result<()> {
        if self.load(provider).is_some() {
            bail!("provider '{}' já registrado no rotator", provider);
        }
        let entry = RotatorEntry {
            provider: provider.to_string(),
            policy,
            current: KeyVersion {
                value: initial_key.to_string(),
                version: 1,
                rotated_at: now_epoch,
            },
            previous: Vec::new(),
        };
        self.save(&entry)
    }

    /// True se a chave do provider excedeu a idade máxima da política.
    pub fn needs_rotation(&self, provider: &str, now_epoch: u64) -> bool {
        match self.load(provider) {
            None => false,
            Some(entry) => {
                let max_age = u64::from(entry.policy.interval_days) * 86_400;
                now_epoch >= entry.current.rotated_at.saturating_add(max_age)
            }
        }
    }

    /// Rotaciona imediatamente: gera chave nova (CSPRNG), preserva a anterior
    /// na janela de versões e emite tupla de auditoria com metadados.
    pub fn rotate_now(&self, provider: &str, now_epoch: u64) -> Result<RotationEvent> {
        let mut entry = self
            .load(provider)
            .with_context(|| format!("provider '{}' não registrado no rotator", provider))?;

        let old_version = entry.current.version;
        let new_version = old_version + 1;

        // Preserva a versão atual no histórico (mais recente primeiro).
        entry.previous.insert(0, entry.current.clone());
        entry.previous.truncate(entry.policy.keep_versions);

        entry.current = KeyVersion {
            value: Self::generate_key(),
            version: new_version,
            rotated_at: now_epoch,
        };
        self.save(&entry)?;

        let event = RotationEvent {
            provider: provider.to_string(),
            old_version,
            new_version,
            rotated_at: now_epoch,
        };

        // Auditoria: metadados apenas — o valor da chave NUNCA sai do vault.
        self.blackboard.put_tuple(
            "audit",
            &format!("vault_rotation:{}:{:06}", provider, new_version),
            serde_json::to_value(&event)?,
        )?;

        Ok(event)
    }

    /// Rotaciona todos os providers cuja política venceu. Ponto de entrada
    /// para o job periódico do scheduler. Determinístico dado `now_epoch`.
    ///
    /// Entradas corrompidas no Blackboard NÃO são silenciadas (regra PVC:
    /// incompleto oculto não pode): cada uma gera tupla de auditoria
    /// `audit::vault_rotator_corrupt:{key}` e a rotação dos demais segue.
    pub fn rotate_due(&self, now_epoch: u64) -> Result<Vec<RotationEvent>> {
        let mut providers: Vec<String> = Vec::new();
        for (key, value) in self.blackboard.search_tuples("vault", "rotator:") {
            match serde_json::from_value::<RotatorEntry>(value) {
                Ok(entry) => providers.push(entry.provider),
                Err(e) => {
                    // Visibilidade obrigatória: entrada ilegível vira auditoria.
                    self.blackboard.put_tuple(
                        "audit",
                        &format!("vault_rotator_corrupt:{}", key),
                        serde_json::json!({
                            "key": key,
                            "error": e.to_string(),
                            "detected_at": now_epoch,
                        }),
                    )?;
                }
            }
        }
        providers.sort(); // ordem determinística

        let mut events = Vec::new();
        for provider in providers {
            if self.needs_rotation(&provider, now_epoch) {
                events.push(self.rotate_now(&provider, now_epoch)?);
            }
        }
        Ok(events)
    }

    /// Chave atual do provider.
    pub fn current_key(&self, provider: &str) -> Option<String> {
        self.load(provider).map(|e| e.current.value)
    }

    /// Versões anteriores retidas (mais recente primeiro) — rollover gradual.
    pub fn previous_versions(&self, provider: &str) -> Vec<KeyVersion> {
        self.load(provider).map(|e| e.previous).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    const DAY: u64 = 86_400;

    #[test]
    fn register_e_current_key() {
        let rotator = AutoRotator::new(temp_bb());
        rotator
            .register("openai", "sk-inicial", RotationPolicy::default(), 1_000)
            .unwrap();
        assert_eq!(rotator.current_key("openai").unwrap(), "sk-inicial");
        assert!(!rotator.needs_rotation("openai", 1_000));
    }

    #[test]
    fn registro_duplicado_falha() {
        let rotator = AutoRotator::new(temp_bb());
        rotator
            .register("openai", "k1", RotationPolicy::default(), 0)
            .unwrap();
        assert!(rotator
            .register("openai", "k2", RotationPolicy::default(), 0)
            .is_err());
    }

    #[test]
    fn needs_rotation_apos_intervalo() {
        let rotator = AutoRotator::new(temp_bb());
        let policy = RotationPolicy {
            interval_days: 30,
            keep_versions: 2,
        };
        rotator.register("openai", "k1", policy, 0).unwrap();
        assert!(!rotator.needs_rotation("openai", 29 * DAY));
        assert!(rotator.needs_rotation("openai", 30 * DAY));
    }

    #[test]
    fn rotate_now_versiona_e_preserva_anterior() {
        let rotator = AutoRotator::new(temp_bb());
        rotator
            .register("openai", "k-v1", RotationPolicy::default(), 0)
            .unwrap();

        let event = rotator.rotate_now("openai", 100).unwrap();
        assert_eq!(event.old_version, 1);
        assert_eq!(event.new_version, 2);

        // Nova chave gerada (CSPRNG, 32 chars), diferente da anterior.
        let current = rotator.current_key("openai").unwrap();
        assert_eq!(current.len(), 32);
        assert_ne!(current, "k-v1");

        // Versão anterior preservada para rollover.
        let previous = rotator.previous_versions("openai");
        assert_eq!(previous.len(), 1);
        assert_eq!(previous[0].value, "k-v1");
        assert_eq!(previous[0].version, 1);
    }

    #[test]
    fn keep_versions_limita_historico() {
        let rotator = AutoRotator::new(temp_bb());
        let policy = RotationPolicy {
            interval_days: 1,
            keep_versions: 2,
        };
        rotator.register("p", "k1", policy, 0).unwrap();
        for i in 1..=4 {
            rotator.rotate_now("p", i * 10).unwrap();
        }
        let previous = rotator.previous_versions("p");
        assert_eq!(previous.len(), 2);
        // Mais recente primeiro: versões 4 e 3.
        assert_eq!(previous[0].version, 4);
        assert_eq!(previous[1].version, 3);
    }

    #[test]
    fn rotate_due_rotaciona_apenas_vencidos() {
        let rotator = AutoRotator::new(temp_bb());
        rotator
            .register(
                "vencido",
                "k1",
                RotationPolicy {
                    interval_days: 1,
                    keep_versions: 1,
                },
                0,
            )
            .unwrap();
        rotator
            .register(
                "novo",
                "k2",
                RotationPolicy {
                    interval_days: 365,
                    keep_versions: 1,
                },
                0,
            )
            .unwrap();

        let events = rotator.rotate_due(2 * DAY).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "vencido");
        // O não vencido permanece na versão 1.
        assert_eq!(rotator.previous_versions("novo").len(), 0);
    }

    #[test]
    fn auditoria_emitida_sem_vazar_chave() {
        let bb = temp_bb();
        let rotator = AutoRotator::new(bb.clone());
        rotator
            .register("openai", "segredo-inicial", RotationPolicy::default(), 0)
            .unwrap();
        rotator.rotate_now("openai", 50).unwrap();

        let audit = bb
            .get_tuple("audit", "vault_rotation:openai:000002")
            .unwrap();
        assert_eq!(audit["provider"], "openai");
        assert_eq!(audit["new_version"], 2);
        // O valor da chave nunca aparece na auditoria.
        let raw = serde_json::to_string(&audit).unwrap();
        assert!(!raw.contains("segredo-inicial"));

        let nova = rotator.current_key("openai").unwrap();
        assert!(!raw.contains(&nova));
    }

    #[test]
    fn estado_sobrevive_a_reabertura() {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        {
            let bb = Blackboard::open(&path).unwrap();
            let rotator = AutoRotator::new(bb);
            rotator
                .register("openai", "k1", RotationPolicy::default(), 0)
                .unwrap();
            rotator.rotate_now("openai", 10).unwrap();
        }
        let bb = Blackboard::open(&path).unwrap();
        let rotator = AutoRotator::new(bb);
        assert_eq!(rotator.previous_versions("openai").len(), 1);
        assert!(rotator.current_key("openai").is_some());
    }

    #[test]
    fn rotate_provider_nao_registrado_falha() {
        let rotator = AutoRotator::new(temp_bb());
        assert!(rotator.rotate_now("fantasma", 0).is_err());
    }

    #[test]
    fn entrada_corrompida_e_auditada_e_nao_bloqueia_demais() {
        let bb = temp_bb();
        let rotator = AutoRotator::new(bb.clone());
        rotator
            .register(
                "valido",
                "k1",
                RotationPolicy {
                    interval_days: 1,
                    keep_versions: 1,
                },
                0,
            )
            .unwrap();
        // Injeta entrada corrompida diretamente no Blackboard.
        bb.put_tuple(
            "vault",
            "rotator:quebrado",
            serde_json::json!({"isto": "não é um RotatorEntry"}),
        )
        .unwrap();

        let events = rotator.rotate_due(2 * DAY).unwrap();
        // O provider válido rotaciona normalmente.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "valido");

        // A corrupção fica visível em auditoria (incompleto oculto não pode).
        let audits = bb.search_tuples("audit", "vault_rotator_corrupt:");
        assert_eq!(audits.len(), 1);
        assert!(audits[0].1["error"].as_str().unwrap().len() > 0);
    }
}
