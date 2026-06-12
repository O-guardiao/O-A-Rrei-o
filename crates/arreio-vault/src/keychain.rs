use anyhow::{Context, Result};
use keyring::Entry;

/// Integração com OS keychain via `keyring`.
/// Utiliza o usuário fixo "arreio-vault" para todas as entradas.
pub struct KeychainStore;

impl KeychainStore {
    /// Salva uma senha/key no keychain do SO para o serviço informado.
    pub fn save(service: &str, key: &str) -> Result<()> {
        let entry = Entry::new(service, "arreio-vault")
            .with_context(|| format!("falha ao criar entrada keyring para {}", service))?;
        entry
            .set_password(key)
            .with_context(|| format!("falha ao salvar key no keyring para {}", service))?;
        Ok(())
    }

    /// Carrega uma senha/key do keychain do SO para o serviço informado.
    pub fn load(service: &str) -> Result<String> {
        let entry = Entry::new(service, "arreio-vault")
            .with_context(|| format!("falha ao criar entrada keyring para {}", service))?;
        let password = entry
            .get_password()
            .with_context(|| format!("falha ao ler key do keyring para {}", service))?;
        Ok(password)
    }

    /// Remove uma entrada do keychain do SO.
    pub fn delete(service: &str) -> Result<()> {
        let entry = Entry::new(service, "arreio-vault")
            .with_context(|| format!("falha ao criar entrada keyring para {}", service))?;
        entry
            .delete_password()
            .with_context(|| format!("falha ao deletar key do keyring para {}", service))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Serializa testes de keychain para evitar race conditions no Windows Credential Manager.
    static KC_LOCK: Mutex<()> = Mutex::new(());

    fn service_unico(nome: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("arreio-vault-test-{}-{}", nome, ts)
    }

    #[test]
    fn save_e_load_roundtrip() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("roundtrip");
        KeychainStore::save(&svc, "secret42").unwrap();
        let loaded = KeychainStore::load(&svc).unwrap();
        assert_eq!(loaded, "secret42");
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn delete_remove_entrada() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("delete");
        KeychainStore::save(&svc, "temp").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        KeychainStore::delete(&svc).unwrap();
        assert!(KeychainStore::load(&svc).is_err());
    }

    #[test]
    fn load_depois_delete_erro() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("load_after_delete");
        KeychainStore::save(&svc, "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        KeychainStore::delete(&svc).unwrap();
        assert!(KeychainStore::load(&svc).is_err());
    }

    #[test]
    fn save_sobrescreve() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("overwrite");
        KeychainStore::save(&svc, "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        KeychainStore::save(&svc, "new").unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), "new");
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn save_vazio() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("empty");
        KeychainStore::save(&svc, "").unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), "");
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn save_unicode() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("unicode");
        let key = "🔐 chave secreta 日本語";
        KeychainStore::save(&svc, key).unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), key);
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn save_grande() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("large");
        // Windows Credential Manager limita atributo password a ~2560 bytes
        let key = "a".repeat(512);
        KeychainStore::save(&svc, &key).unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), key);
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn multiplos_services_independentes() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc1 = service_unico("multi1");
        let svc2 = service_unico("multi2");
        KeychainStore::save(&svc1, "key1").unwrap();
        KeychainStore::save(&svc2, "key2").unwrap();
        assert_eq!(KeychainStore::load(&svc1).unwrap(), "key1");
        assert_eq!(KeychainStore::load(&svc2).unwrap(), "key2");
        KeychainStore::delete(&svc1).unwrap();
        KeychainStore::delete(&svc2).unwrap();
    }

    #[test]
    fn service_com_pontos() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("dots");
        let svc = svc.replace("-", ".");
        KeychainStore::save(&svc, "val").unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), "val");
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn service_com_espacos() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("spaces");
        let svc = svc.replace("-", " ");
        KeychainStore::save(&svc, "val").unwrap();
        assert_eq!(KeychainStore::load(&svc).unwrap(), "val");
        KeychainStore::delete(&svc).unwrap();
    }

    #[test]
    fn load_service_inexistente_erro() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("nonexistent");
        assert!(KeychainStore::load(&svc).is_err());
    }

    #[test]
    fn delete_service_inexistente_erro() {
        let _guard = KC_LOCK.lock().unwrap();
        let svc = service_unico("nonexistent_del");
        assert!(KeychainStore::delete(&svc).is_err());
    }
}
