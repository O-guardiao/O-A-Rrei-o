//! AgentCredential — identidade zero-trust por agente (PVC-Q3.2).
//!
//! Cada ator/bridge recebe uma credencial assinada (JWT HMAC-SHA256, reusa
//! `jwt.rs` — sem dependências novas) carregando **capability scopes**:
//! permissões fine-grained no formato `domínio:ação[:alvo]`, ex.:
//! `tool:read_file`, `tool:*`, `vault:read:openai`, `dag:execute`.
//!
//! Princípios zero-trust aplicados:
//! - **Deny-by-default**: credencial sem scope correspondente → negado.
//! - **Least-privilege scoped a invocation**: o `ToolPolicyPipeline` verifica
//!   o scope a cada chamada de tool, não por sessão.
//! - **Sem ambient authority**: a credencial viaja com a invocação; expirou,
//!   acabou o acesso.

use crate::jwt::{issue_token_with_secret, verify_token_with_secret, JwtError};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Claim extra que marca o JWT como credencial de agente.
const CREDENTIAL_TYPE_CLAIM: &str = "arreio_cred";
const CREDENTIAL_TYPE_VALUE: &str = "agent_credential_v1";

// ── CapabilityScope ───────────────────────────────────────────────────────────

/// Scope de capability: segmentos separados por `:`.
///
/// Regras de matching (determinísticas, deny-by-default):
/// - segmento `*` casa com qualquer segmento;
/// - segmento `abc*` casa por prefixo dentro do próprio segmento;
/// - um scope com MENOS segmentos que a capability requisitada só concede
///   acesso se seu último segmento for exatamente `*` (cobre a subárvore):
///   `vault:*` concede `vault:read:openai`, mas `vault:read*` NÃO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityScope {
    raw: String,
}

impl CapabilityScope {
    /// Valida e constrói um scope. Segmentos vazios e `*` fora de posição
    /// final de segmento são rejeitados.
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            bail!("scope vazio");
        }
        for seg in raw.split(':') {
            if seg.is_empty() {
                bail!("scope '{}' tem segmento vazio", raw);
            }
            // '*' só pode aparecer como último caractere do segmento.
            if let Some(pos) = seg.find('*') {
                if pos != seg.len() - 1 {
                    bail!("scope '{}': '*' só é válido no fim do segmento", raw);
                }
            }
        }
        Ok(Self {
            raw: raw.to_string(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// True se este scope concede a capability requisitada.
    pub fn grants(&self, requested: &str) -> bool {
        let scope_segs: Vec<&str> = self.raw.split(':').collect();
        let req_segs: Vec<&str> = requested.split(':').collect();

        // Scope mais específico que o pedido nunca concede.
        if scope_segs.len() > req_segs.len() {
            return false;
        }

        for (i, seg) in scope_segs.iter().enumerate() {
            let req = req_segs[i];
            let seg_match = if *seg == "*" {
                true
            } else if let Some(prefix) = seg.strip_suffix('*') {
                req.starts_with(prefix)
            } else {
                *seg == req
            };
            if !seg_match {
                return false;
            }
        }

        // Scope mais curto que o pedido: só cobre o resto se terminar em "*".
        if scope_segs.len() < req_segs.len() {
            return scope_segs.last() == Some(&"*");
        }
        true
    }
}

// ── AgentCredential ───────────────────────────────────────────────────────────

/// Credencial verificada de um agente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCredential {
    /// Identificador do agente (claim `sub`).
    pub agent_id: String,
    /// Papel do agente (claim `prf`): architect, developer, inspector, bridge...
    pub role: String,
    /// Capability scopes concedidos (claim `prm`).
    pub scopes: Vec<CapabilityScope>,
    /// Expiração (epoch segundos).
    pub expires_at: u64,
    /// ID único do token (para auditoria e revogação futura).
    pub jti: String,
}

impl AgentCredential {
    /// Emite uma credencial assinada. Scopes inválidos são rejeitados na
    /// emissão (nunca circulam tokens com scopes malformados).
    pub fn issue_with_secret(
        agent_id: &str,
        role: &str,
        scopes: &[&str],
        ttl_hours: u64,
        secret: &str,
    ) -> Result<String> {
        // Valida todos os scopes antes de emitir.
        for s in scopes {
            CapabilityScope::parse(s)?;
        }
        let prm: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        let token = issue_token_with_secret(
            agent_id,
            role,
            &prm,
            ttl_hours,
            &[(CREDENTIAL_TYPE_CLAIM, CREDENTIAL_TYPE_VALUE)],
            secret,
        )?;
        Ok(token)
    }

    /// Verifica um token e reconstrói a credencial. Falha se a assinatura
    /// for inválida, o token estiver expirado ou algum scope for malformado.
    pub fn verify_with_secret(token: &str, secret: &str) -> Result<Self> {
        let claims = verify_token_with_secret(token, secret)?;
        let mut scopes = Vec::with_capacity(claims.prm.len());
        for s in &claims.prm {
            scopes.push(CapabilityScope::parse(s)?);
        }
        Ok(Self {
            agent_id: claims.sub,
            role: claims.prf,
            scopes,
            expires_at: claims.exp,
            jti: claims.jti,
        })
    }

    /// True se a credencial expirou em relação a `now_epoch`.
    pub fn is_expired(&self, now_epoch: u64) -> bool {
        now_epoch >= self.expires_at
    }

    /// Deny-by-default: true somente se ALGUM scope concede a capability.
    pub fn authorizes(&self, capability: &str) -> bool {
        self.scopes.iter().any(|s| s.grants(capability))
    }

    /// Conveniência: autorização de tool (`tool:{nome}`).
    pub fn authorizes_tool(&self, tool_name: &str) -> bool {
        self.authorizes(&format!("tool:{}", tool_name))
    }
}

/// Erro re-exportado para conveniência de chamadores que tratam JWT.
pub type CredentialJwtError = JwtError;

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a-very-secure-secret-key-for-testing-32chars";

    #[test]
    fn scope_exato_concede() {
        let s = CapabilityScope::parse("tool:read_file").unwrap();
        assert!(s.grants("tool:read_file"));
        assert!(!s.grants("tool:write_file"));
    }

    #[test]
    fn scope_curinga_de_segmento() {
        let s = CapabilityScope::parse("tool:*").unwrap();
        assert!(s.grants("tool:read_file"));
        assert!(s.grants("tool:exec"));
        assert!(!s.grants("vault:read"));
    }

    #[test]
    fn curinga_cobre_subarvore_apenas_se_ultimo_segmento_for_asterisco() {
        let cobre = CapabilityScope::parse("vault:*").unwrap();
        assert!(cobre.grants("vault:read:openai"));

        let nao_cobre = CapabilityScope::parse("vault:read*").unwrap();
        assert!(nao_cobre.grants("vault:read"));
        assert!(nao_cobre.grants("vault:readonly"));
        assert!(!nao_cobre.grants("vault:read:openai"));
    }

    #[test]
    fn prefixo_dentro_do_segmento() {
        let s = CapabilityScope::parse("vault:read:openai*").unwrap();
        assert!(s.grants("vault:read:openai"));
        assert!(s.grants("vault:read:openai-prod"));
        assert!(!s.grants("vault:read:anthropic"));
    }

    #[test]
    fn scope_mais_especifico_que_pedido_nao_concede() {
        let s = CapabilityScope::parse("vault:read:openai").unwrap();
        assert!(!s.grants("vault:read"));
    }

    #[test]
    fn scope_admin_total() {
        let s = CapabilityScope::parse("*").unwrap();
        assert!(s.grants("tool:exec"));
        assert!(s.grants("vault:read:openai"));
    }

    #[test]
    fn scopes_malformados_sao_rejeitados() {
        assert!(CapabilityScope::parse("").is_err());
        assert!(CapabilityScope::parse("tool::x").is_err());
        assert!(CapabilityScope::parse("tool:*read").is_err());
        assert!(CapabilityScope::parse(":tool").is_err());
    }

    #[test]
    fn issue_verify_roundtrip() {
        let token = AgentCredential::issue_with_secret(
            "agent-dev-01",
            "developer",
            &["tool:read_file", "tool:grep_search", "vault:read:openai*"],
            1,
            SECRET,
        )
        .unwrap();
        let cred = AgentCredential::verify_with_secret(&token, SECRET).unwrap();
        assert_eq!(cred.agent_id, "agent-dev-01");
        assert_eq!(cred.role, "developer");
        assert_eq!(cred.scopes.len(), 3);
        assert!(!cred.jti.is_empty());
    }

    #[test]
    fn deny_by_default() {
        let token =
            AgentCredential::issue_with_secret("a", "guest", &["tool:read_file"], 1, SECRET)
                .unwrap();
        let cred = AgentCredential::verify_with_secret(&token, SECRET).unwrap();
        assert!(cred.authorizes_tool("read_file"));
        assert!(!cred.authorizes_tool("write_file"));
        assert!(!cred.authorizes("vault:read:openai"));
    }

    #[test]
    fn credencial_sem_scopes_nao_autoriza_nada() {
        let token = AgentCredential::issue_with_secret("a", "guest", &[], 1, SECRET).unwrap();
        let cred = AgentCredential::verify_with_secret(&token, SECRET).unwrap();
        assert!(!cred.authorizes_tool("read_file"));
        assert!(!cred.authorizes("*"));
    }

    #[test]
    fn token_adulterado_falha_verificacao() {
        let token =
            AgentCredential::issue_with_secret("a", "developer", &["tool:*"], 1, SECRET).unwrap();
        let tampered = format!("{}x", token);
        assert!(AgentCredential::verify_with_secret(&tampered, SECRET).is_err());
    }

    #[test]
    fn secret_errado_falha_verificacao() {
        let token =
            AgentCredential::issue_with_secret("a", "developer", &["tool:*"], 1, SECRET).unwrap();
        let wrong = "another-very-secure-secret-key-32-chars!";
        assert!(AgentCredential::verify_with_secret(&token, wrong).is_err());
    }

    #[test]
    fn emissao_rejeita_scope_invalido() {
        assert!(
            AgentCredential::issue_with_secret("a", "developer", &["tool::bad"], 1, SECRET)
                .is_err()
        );
    }

    #[test]
    fn expiracao_e_detectada() {
        let token =
            AgentCredential::issue_with_secret("a", "developer", &["tool:*"], 1, SECRET).unwrap();
        let cred = AgentCredential::verify_with_secret(&token, SECRET).unwrap();
        assert!(!cred.is_expired(cred.expires_at - 10));
        assert!(cred.is_expired(cred.expires_at));
        assert!(cred.is_expired(cred.expires_at + 10));
    }
}
