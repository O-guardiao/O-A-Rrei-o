use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

/// Conjunto de roles disponíveis no sistema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    Admin,
    Developer,
    Auditor,
    Guest,
}

/// Conjunto de permissões que podem ser concedidas a roles ou usuários.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Permission {
    BlackboardRead,
    BlackboardWrite,
    DagCreate,
    DagExecute,
    CheckpointRollback,
    HypervisorExecute,
    McpToolUse,
    McpResourceRead,
    AuditRead,
    UserManage,
    /// Aprovar decisões HITL (PVC-Q1.2).
    HitlApprove,
    /// Rejeitar decisões HITL (PVC-Q1.2).
    HitlReject,
    /// Ler todas as decisões HITL pendentes (PVC-Q1.2).
    HitlReadAll,
}

/// Motor de controle de acesso baseado em roles (RBAC).
///
/// Mantém dois mapeamentos:
/// - `role_permissions`: qual conjunto de permissões cada role possui.
/// - `user_roles`: quais roles cada usuário possui.
pub struct RbacEngine {
    role_permissions: HashMap<Role, HashSet<Permission>>,
    user_roles: HashMap<String, HashSet<Role>>,
}

impl Default for RbacEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RbacEngine {
    /// Cria um motor RBAC vazio, sem permissões ou usuários configurados.
    pub fn new() -> Self {
        Self {
            role_permissions: HashMap::new(),
            user_roles: HashMap::new(),
        }
    }

    /// Inicializa o motor com as permissões padrão do sistema:
    ///
    /// - **Admin**: todas as permissões.
    /// - **Developer**: BlackboardRead/Write, DagCreate/Execute, McpToolUse, McpResourceRead.
    /// - **Auditor**: BlackboardRead, AuditRead, McpResourceRead.
    /// - **Guest**: BlackboardRead, McpResourceRead.
    pub fn with_defaults() -> Self {
        let mut engine = Self::new();

        let all_permissions = HashSet::from([
            Permission::BlackboardRead,
            Permission::BlackboardWrite,
            Permission::DagCreate,
            Permission::DagExecute,
            Permission::CheckpointRollback,
            Permission::HypervisorExecute,
            Permission::McpToolUse,
            Permission::McpResourceRead,
            Permission::AuditRead,
            Permission::UserManage,
            Permission::HitlApprove,
            Permission::HitlReject,
            Permission::HitlReadAll,
        ]);
        engine.role_permissions.insert(Role::Admin, all_permissions);

        let dev_permissions = HashSet::from([
            Permission::BlackboardRead,
            Permission::BlackboardWrite,
            Permission::DagCreate,
            Permission::DagExecute,
            Permission::HitlApprove,
            Permission::McpToolUse,
            Permission::McpResourceRead,
        ]);
        engine
            .role_permissions
            .insert(Role::Developer, dev_permissions);

        let auditor_permissions = HashSet::from([
            Permission::BlackboardRead,
            Permission::AuditRead,
            Permission::McpResourceRead,
            Permission::HitlReadAll,
        ]);
        engine
            .role_permissions
            .insert(Role::Auditor, auditor_permissions);

        let guest_permissions =
            HashSet::from([Permission::BlackboardRead, Permission::McpResourceRead]);
        engine
            .role_permissions
            .insert(Role::Guest, guest_permissions);

        engine
    }

    /// Atribui uma role a um usuário.
    ///
    /// # Erros
    /// Retorna erro se a role não possuir permissões registradas no motor.
    pub fn assign_role(&mut self, user: &str, role: Role) -> Result<()> {
        if !self.role_permissions.contains_key(&role) {
            bail!("role não possui permissões registradas no motor");
        }
        self.user_roles
            .entry(user.to_string())
            .or_default()
            .insert(role);
        Ok(())
    }

    /// Revoga uma role de um usuário.
    ///
    /// # Erros
    /// Retorna erro se o usuário não possuir a role informada.
    pub fn revoke_role(&mut self, user: &str, role: Role) -> Result<()> {
        match self.user_roles.get_mut(user) {
            Some(roles) if roles.contains(&role) => {
                roles.remove(&role);
                if roles.is_empty() {
                    self.user_roles.remove(user);
                }
                Ok(())
            }
            _ => bail!("usuário não possui a role informada"),
        }
    }

    /// Verifica se o usuário possui uma determinada permissão,
    /// diretamente ou através de qualquer uma de suas roles.
    pub fn has_permission(&self, user: &str, permission: &Permission) -> bool {
        self.user_permissions(user).contains(permission)
    }

    /// Retorna o conjunto agregado de permissões de um usuário,
    /// unindo todas as permissões das roles atribuídas.
    pub fn user_permissions(&self, user: &str) -> HashSet<Permission> {
        let mut perms = HashSet::new();
        if let Some(roles) = self.user_roles.get(user) {
            for role in roles {
                if let Some(role_perms) = self.role_permissions.get(role) {
                    perms.extend(role_perms.iter().cloned());
                }
            }
        }
        perms
    }

    /// Retorna o conjunto de roles atribuídas a um usuário.
    pub fn user_roles(&self, user: &str) -> HashSet<Role> {
        self.user_roles.get(user).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1. Motor vazio não possui permissões para usuário desconhecido.
    #[test]
    fn usuario_desconhecido_nao_tem_permissao() {
        let engine = RbacEngine::new();
        assert!(!engine.has_permission("joao", &Permission::BlackboardRead));
    }

    // 2. Motor com defaults possui permissões corretas para Admin.
    #[test]
    fn admin_possui_todas_permissoes() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("admin1", Role::Admin).unwrap();

        let all = Permission::BlackboardRead;
        assert!(engine.has_permission("admin1", &all));
        assert!(engine.has_permission("admin1", &Permission::UserManage));
        assert!(engine.has_permission("admin1", &Permission::CheckpointRollback));
    }

    // 3. Developer possui permissões esperadas e não possui permissões de admin.
    #[test]
    fn developer_possui_permissoes_corretas() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("dev1", Role::Developer).unwrap();

        assert!(engine.has_permission("dev1", &Permission::BlackboardRead));
        assert!(engine.has_permission("dev1", &Permission::BlackboardWrite));
        assert!(engine.has_permission("dev1", &Permission::DagCreate));
        assert!(engine.has_permission("dev1", &Permission::DagExecute));
        assert!(engine.has_permission("dev1", &Permission::McpToolUse));
        assert!(engine.has_permission("dev1", &Permission::McpResourceRead));

        assert!(!engine.has_permission("dev1", &Permission::CheckpointRollback));
        assert!(!engine.has_permission("dev1", &Permission::HypervisorExecute));
        assert!(!engine.has_permission("dev1", &Permission::AuditRead));
        assert!(!engine.has_permission("dev1", &Permission::UserManage));
    }

    // 4. Auditor possui apenas permissões de leitura e auditoria.
    #[test]
    fn auditor_possui_apenas_leitura_e_auditoria() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("aud1", Role::Auditor).unwrap();

        assert!(engine.has_permission("aud1", &Permission::BlackboardRead));
        assert!(engine.has_permission("aud1", &Permission::AuditRead));
        assert!(engine.has_permission("aud1", &Permission::McpResourceRead));

        assert!(!engine.has_permission("aud1", &Permission::BlackboardWrite));
        assert!(!engine.has_permission("aud1", &Permission::DagCreate));
        assert!(!engine.has_permission("aud1", &Permission::DagExecute));
    }

    // 5. Guest possui apenas permissões de leitura.
    #[test]
    fn guest_possui_apenas_leitura() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("guest1", Role::Guest).unwrap();

        assert!(engine.has_permission("guest1", &Permission::BlackboardRead));
        assert!(engine.has_permission("guest1", &Permission::McpResourceRead));

        assert!(!engine.has_permission("guest1", &Permission::BlackboardWrite));
        assert!(!engine.has_permission("guest1", &Permission::McpToolUse));
    }

    // 6. Usuário com múltiplas roles acumula permissões.
    #[test]
    fn multiplas_roles_acumulam_permissoes() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("usuario", Role::Guest).unwrap();
        engine.assign_role("usuario", Role::Developer).unwrap();

        assert!(engine.has_permission("usuario", &Permission::BlackboardRead));
        assert!(engine.has_permission("usuario", &Permission::BlackboardWrite));
        assert!(engine.has_permission("usuario", &Permission::McpResourceRead));
    }

    // 7. Revogação de role remove permissões associadas.
    #[test]
    fn revogar_role_remove_permissoes() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("dev1", Role::Developer).unwrap();
        assert!(engine.has_permission("dev1", &Permission::DagCreate));

        engine.revoke_role("dev1", Role::Developer).unwrap();
        assert!(!engine.has_permission("dev1", &Permission::DagCreate));
    }

    // 8. Revogar role inexistente deve retornar erro.
    #[test]
    fn revogar_role_inexistente_falha() {
        let mut engine = RbacEngine::with_defaults();
        let result = engine.revoke_role("ninguem", Role::Admin);
        assert!(result.is_err());
    }

    // 9. Atribuir role não registrada no motor deve retornar erro.
    #[test]
    fn atribuir_role_nao_registrada_falha() {
        let mut engine = RbacEngine::new(); // sem defaults
        let result = engine.assign_role("user", Role::Admin);
        assert!(result.is_err());
    }

    // 10. user_roles retorna conjunto correto.
    #[test]
    fn lista_roles_de_usuario() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("mix", Role::Auditor).unwrap();
        engine.assign_role("mix", Role::Guest).unwrap();

        let roles = engine.user_roles("mix");
        assert_eq!(roles.len(), 2);
        assert!(roles.contains(&Role::Auditor));
        assert!(roles.contains(&Role::Guest));
    }

    // 11. user_permissions retorna união correta.
    #[test]
    fn lista_permissoes_de_usuario() {
        let mut engine = RbacEngine::with_defaults();
        engine.assign_role("auditor", Role::Auditor).unwrap();

        let perms = engine.user_permissions("auditor");
        assert!(perms.contains(&Permission::BlackboardRead));
        assert!(perms.contains(&Permission::AuditRead));
        assert!(perms.contains(&Permission::McpResourceRead));
        assert!(perms.contains(&Permission::HitlReadAll));
        assert_eq!(perms.len(), 4);
    }

    // 12. user_roles de usuário inexistente retorna conjunto vazio.
    #[test]
    fn usuario_inexistente_retorna_roles_vazio() {
        let engine = RbacEngine::with_defaults();
        assert!(engine.user_roles("fantasma").is_empty());
    }
}
