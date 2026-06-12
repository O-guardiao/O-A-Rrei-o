//! Cache de estado para restauração durante tentativas de recovery.
//!
//! Salva snapshots do sistema em arquivos temporários antes de cada tentativa,
//! permitindo rollback controlado entre alternativas de modelos.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Estrutura responsável por persistir e recuperar estados do sistema.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryCache {
    dir: PathBuf,
}

impl RecoveryCache {
    /// Cria um novo cache apontando para o diretório fornecido.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Salva um estado associado a uma chave.
    /// Sobrescreve caso a chave já exista.
    pub fn save_state(&mut self, key: &str, state: &str) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("falha ao criar diretório do cache: {:?}", self.dir))?;
        let path = self.dir.join(format!("{}.json", key));
        let mut file = fs::File::create(&path)
            .with_context(|| format!("falha ao criar arquivo de cache: {:?}", path))?;
        file.write_all(state.as_bytes())
            .with_context(|| format!("falha ao escrever estado no cache: {:?}", path))?;
        Ok(())
    }

    /// Restaura o estado previamente salvo para a chave fornecida.
    pub fn restore_state(&self, key: &str) -> Result<String> {
        let path = self.dir.join(format!("{}.json", key));
        let content = fs::read_to_string(&path)
            .with_context(|| format!("falha ao ler arquivo de cache: {:?}", path))?;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_recovery_cache_save_restore_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let mut cache = RecoveryCache::new(temp_dir.path().to_path_buf());

        cache.save_state("attempt_1", r#"{"status":"ok"}"#).unwrap();
        let restored = cache.restore_state("attempt_1").unwrap();

        assert_eq!(restored, r#"{"status":"ok"}"#);
    }

    #[test]
    fn test_recovery_cache_restore_missing_key_fails() {
        let temp_dir = TempDir::new().unwrap();
        let cache = RecoveryCache::new(temp_dir.path().to_path_buf());

        assert!(cache.restore_state("missing").is_err());
    }

    #[test]
    fn test_recovery_cache_overwrite_key() {
        let temp_dir = TempDir::new().unwrap();
        let mut cache = RecoveryCache::new(temp_dir.path().to_path_buf());

        cache.save_state("key", "first").unwrap();
        cache.save_state("key", "second").unwrap();
        let restored = cache.restore_state("key").unwrap();

        assert_eq!(restored, "second");
    }
}
