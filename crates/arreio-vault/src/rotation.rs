use anyhow::Result;
use std::collections::HashMap;

/// Metadados de uma API key armazenada no rotator.
#[derive(Debug, Clone)]
pub struct KeyMetadata {
    pub provider: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub rotation_interval_days: u32,
}

/// Rotação automática de keys.
/// Mantém metadados sobre quando cada provider foi criado e quando deve ser rotacionado.
pub struct KeyRotator {
    keys: HashMap<String, KeyMetadata>,
    key_values: HashMap<String, String>,
}

impl KeyRotator {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
            key_values: HashMap::new(),
        }
    }

    /// Registra um provider com intervalo de rotação em dias.
    pub fn register(&mut self, provider: &str, interval_days: u32) {
        let now = now();
        self.keys.insert(
            provider.to_string(),
            KeyMetadata {
                provider: provider.to_string(),
                created_at: now,
                expires_at: None,
                rotation_interval_days: interval_days,
            },
        );
    }

    /// Verifica se o provider precisa de rotação (intervalo expirou ou `expires_at` passou).
    pub fn needs_rotation(&self, provider: &str) -> bool {
        let Some(meta) = self.keys.get(provider) else {
            return false;
        };
        let now = now();
        if let Some(exp) = meta.expires_at {
            if now >= exp {
                return true;
            }
        }
        let threshold = meta
            .created_at
            .saturating_add(u64::from(meta.rotation_interval_days) * 86400);
        now >= threshold
    }

    /// Executa rotação: atualiza `created_at`, limpa `expires_at` e armazena a nova key.
    pub fn rotate(&mut self, provider: &str, new_key: &str) -> Result<()> {
        let meta = self
            .keys
            .get_mut(provider)
            .ok_or_else(|| anyhow::anyhow!("provider '{}' não registrado", provider))?;
        meta.created_at = now();
        meta.expires_at = None;
        self.key_values
            .insert(provider.to_string(), new_key.to_string());
        Ok(())
    }

    /// Lista providers que vão expirar nos próximos `days` dias.
    pub fn list_expiring(&self, days: u32) -> Vec<String> {
        let now = now();
        let window_secs = u64::from(days) * 86400;
        let mut result: Vec<String> = self
            .keys
            .values()
            .filter(|meta| {
                let threshold = meta
                    .created_at
                    .saturating_add(u64::from(meta.rotation_interval_days) * 86400);
                // Considera expirando se threshold está no futuro mas dentro da janela,
                // ou se já expirou (threshold <= now) — neste caso também lista.
                threshold <= now + window_secs
            })
            .map(|meta| meta.provider.clone())
            .collect();
        result.sort();
        result
    }

    /// Retorna a key atual de um provider (se houver).
    pub fn get_key(&self, provider: &str) -> Option<&str> {
        self.key_values.get(provider).map(|s| s.as_str())
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

    #[test]
    fn register_e_list() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 30);
        assert!(rotator.keys.contains_key("openai"));
        assert_eq!(rotator.keys["openai"].rotation_interval_days, 30);
    }

    #[test]
    fn needs_rotation_novo_false() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 30);
        assert!(!rotator.needs_rotation("openai"));
    }

    #[test]
    fn needs_rotation_antigo_true() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 1);
        // Simula created_at no passado
        rotator.keys.get_mut("openai").unwrap().created_at = now() - 2 * 86400;
        assert!(rotator.needs_rotation("openai"));
    }

    #[test]
    fn rotate_atualiza_timestamps() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 30);
        let old_created = rotator.keys["openai"].created_at;
        // dorme 1s para garantir diferença de timestamp
        std::thread::sleep(std::time::Duration::from_secs(1));
        rotator.rotate("openai", "new-key").unwrap();
        let meta = &rotator.keys["openai"];
        assert!(meta.created_at > old_created);
        assert!(meta.expires_at.is_none());
    }

    #[test]
    fn rotate_armazena_key() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 30);
        rotator.rotate("openai", "sk-nova").unwrap();
        assert_eq!(rotator.get_key("openai"), Some("sk-nova"));
    }

    #[test]
    fn list_expiring_vazio() {
        let rotator = KeyRotator::new();
        assert!(rotator.list_expiring(30).is_empty());
    }

    #[test]
    fn list_expiring_com_dias() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 1);
        // criado há 2 dias -> já expirou, deve aparecer em qualquer janela >=0
        rotator.keys.get_mut("openai").unwrap().created_at = now() - 2 * 86400;
        let expiring = rotator.list_expiring(0);
        assert_eq!(expiring, vec!["openai"]);
    }

    #[test]
    fn provider_inexistente_needs_rotation_false() {
        let rotator = KeyRotator::new();
        assert!(!rotator.needs_rotation("inexistente"));
    }

    #[test]
    fn register_sobrescreve() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 10);
        let first = rotator.keys["openai"].created_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        rotator.register("openai", 20);
        let second = rotator.keys["openai"].created_at;
        assert!(second > first || second >= first);
        assert_eq!(rotator.keys["openai"].rotation_interval_days, 20);
    }

    #[test]
    fn expires_at_setado_manualmente() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 365);
        // Define expiração no passado
        rotator.keys.get_mut("openai").unwrap().expires_at = Some(now() - 1);
        assert!(rotator.needs_rotation("openai"));
    }

    #[test]
    fn list_expiring_multiplos_providers() {
        let mut rotator = KeyRotator::new();
        rotator.register("a", 1);
        rotator.register("b", 365);
        rotator.keys.get_mut("a").unwrap().created_at = now() - 2 * 86400;
        let expiring = rotator.list_expiring(30);
        assert_eq!(expiring, vec!["a"]);
    }

    #[test]
    fn rotate_provider_inexistente_erro() {
        let mut rotator = KeyRotator::new();
        assert!(rotator.rotate("inexistente", "key").is_err());
    }

    #[test]
    fn get_key_sem_rotacao_none() {
        let mut rotator = KeyRotator::new();
        rotator.register("openai", 30);
        assert_eq!(rotator.get_key("openai"), None);
    }
}
