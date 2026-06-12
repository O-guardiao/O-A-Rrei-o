use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{bail, Context, Result};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use std::collections::HashMap;

/// Número de iterações PBKDF2 para derivação de chave.
const PBKDF2_ROUNDS: u32 = 100_000;
/// Tamanho do nonce AES-GCM em bytes.
const NONCE_SIZE: usize = 12;
/// Tamanho da chave AES-256 em bytes.
const KEY_SIZE: usize = 32;

/// Storage criptografado AES-256-GCM para API keys.
/// Cada provider possui sua chave armazenada como `nonce || ciphertext`.
pub struct ApiKeyStore {
    cipher: Aes256Gcm,
    keys: HashMap<String, Vec<u8>>,
}

impl ApiKeyStore {
    /// Deriva chave master de uma senha via PBKDF2-HMAC-SHA256.
    pub fn from_password(password: &str, salt: &[u8; 16]) -> Result<Self> {
        let mut key_bytes = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ROUNDS, &mut key_bytes);
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        Ok(Self {
            cipher,
            keys: HashMap::new(),
        })
    }

    /// Armazena API key criptografada para um provider.
    pub fn store(&mut self, provider: &str, key: &str) -> Result<()> {
        let nonce_bytes: [u8; NONCE_SIZE] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, key.as_bytes())
            .map_err(|e| anyhow::anyhow!("falha ao criptografar: {:?}", e))?;
        let mut stored = nonce_bytes.to_vec();
        stored.extend_from_slice(&ciphertext);
        self.keys.insert(provider.to_string(), stored);
        Ok(())
    }

    /// Recupera API key descriptografada de um provider.
    pub fn retrieve(&self, provider: &str) -> Result<String> {
        let stored = self
            .keys
            .get(provider)
            .with_context(|| format!("provider '{}' não encontrado", provider))?;
        if stored.len() < NONCE_SIZE {
            bail!("dados corrompidos: menor que nonce");
        }
        let (nonce_bytes, ciphertext) = stored.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("falha ao descriptografar: {:?}", e))?;
        String::from_utf8(plaintext).with_context(|| "plaintext inválido UTF-8")
    }

    /// Lista providers com chaves armazenadas.
    pub fn list_providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self.keys.keys().cloned().collect();
        providers.sort();
        providers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_senha(senha: &str) -> ApiKeyStore {
        let salt: [u8; 16] = rand::random();
        ApiKeyStore::from_password(senha, &salt).unwrap()
    }

    #[test]
    fn from_password_cria_store() {
        let salt: [u8; 16] = rand::random();
        let store = ApiKeyStore::from_password("senha forte", &salt);
        assert!(store.is_ok());
    }

    #[test]
    fn store_e_retrieve_roundtrip() {
        let mut store = store_senha("master123");
        store.store("openai", "sk-abc123").unwrap();
        let recovered = store.retrieve("openai").unwrap();
        assert_eq!(recovered, "sk-abc123");
    }

    #[test]
    fn retrieve_provider_inexistente() {
        let store = store_senha("master123");
        let res = store.retrieve("inexistente");
        assert!(res.is_err());
    }

    #[test]
    fn list_providers_vazia() {
        let store = store_senha("master123");
        assert!(store.list_providers().is_empty());
    }

    #[test]
    fn list_providers_ordenada() {
        let mut store = store_senha("master123");
        store.store("zeta", "k1").unwrap();
        store.store("alpha", "k2").unwrap();
        store.store("beta", "k3").unwrap();
        let list = store.list_providers();
        assert_eq!(list, vec!["alpha", "beta", "zeta"]);
    }

    #[test]
    fn store_multiplas_keys() {
        let mut store = store_senha("master123");
        store.store("openai", "sk-openai").unwrap();
        store.store("anthropic", "sk-anthropic").unwrap();
        assert_eq!(store.retrieve("openai").unwrap(), "sk-openai");
        assert_eq!(store.retrieve("anthropic").unwrap(), "sk-anthropic");
    }

    #[test]
    fn store_sobrescreve() {
        let mut store = store_senha("master123");
        store.store("openai", "old-key").unwrap();
        store.store("openai", "new-key").unwrap();
        assert_eq!(store.retrieve("openai").unwrap(), "new-key");
    }

    #[test]
    fn salt_diferente_gera_chave_diferente() {
        let salt1: [u8; 16] = rand::random();
        let salt2: [u8; 16] = rand::random();
        let mut store1 = ApiKeyStore::from_password("mesma_senha", &salt1).unwrap();
        let mut store2 = ApiKeyStore::from_password("mesma_senha", &salt2).unwrap();
        store1.store("p", "key").unwrap();
        store2.store("p", "key").unwrap();
        // Ciphertexts devem ser diferentes devido ao salt e nonce.
        assert_ne!(store1.keys["p"], store2.keys["p"]);
    }

    #[test]
    fn nonce_unico_por_store() {
        let mut store = store_senha("master123");
        store.store("p1", "key").unwrap();
        store.store("p2", "key").unwrap();
        assert_ne!(store.keys["p1"], store.keys["p2"]);
    }

    #[test]
    fn chave_vazia() {
        let mut store = store_senha("master123");
        store.store("empty", "").unwrap();
        assert_eq!(store.retrieve("empty").unwrap(), "");
    }

    #[test]
    fn senha_vazia() {
        let salt: [u8; 16] = rand::random();
        let mut store = ApiKeyStore::from_password("", &salt).unwrap();
        store.store("p", "val").unwrap();
        assert_eq!(store.retrieve("p").unwrap(), "val");
    }

    #[test]
    fn senha_errada_nao_descriptografa() {
        let salt: [u8; 16] = rand::random();
        let mut store1 = ApiKeyStore::from_password("certa", &salt).unwrap();
        let store2 = ApiKeyStore::from_password("errada", &salt).unwrap();
        store1.store("p", "secreto").unwrap();
        // Simula dados copiados para outro store com senha errada
        let mut store2_with_data = store2;
        store2_with_data
            .keys
            .insert("p".into(), store1.keys["p"].clone());
        let res = store2_with_data.retrieve("p");
        assert!(res.is_err());
    }
}
