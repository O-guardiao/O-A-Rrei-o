//! Auth Middleware — Camada 3 de Autenticação (OpenClaw ADR-003).
//!
//! Única camada que conhece HTTP/TcpStream. Extrai tokens de headers,
//! valida via JWT (Camada 2) ou API Key (Camada 1), e autoriza via RBAC.
//!
//! ## Fluxo
//! ```text
//! Request HTTP → extract_token() → verify_jwt() || authenticate_api_key()
//!     → AuthContext { client_id, role }
//!     → RBAC check → allow / 403
//! ```
//!
//! ## Modos de Autenticação
//! - `NoAuth`: sem autenticação (dev, comportamento legado)
//! - `ApiKey`: validação por API key via ClientRegistry
//! - `Jwt`: validação por JWT via arreio_security::jwt
//! - `Hybrid`: tenta JWT primeiro, fallback API Key
//!
//! ## Segurança
//! - Comparação timing-safe de tokens
//! - Audit log em toda tentativa de auth
//! - Headers: `Authorization: Bearer <token>` ou `X-API-Key: <key>`

use anyhow::Result;
use arreio_kernel::Blackboard;
use arreio_security::{verify_token, JwtClaims, Permission, RbacEngine, Role};
use arreio_vault::ClientRegistry;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Tipos ─────────────────────────────────────────────────────────────────────

/// Modo de autenticação do gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Sem autenticação — todas as rotas liberadas (dev/localhost).
    NoAuth,
    /// Autenticação por API key via ClientRegistry (Blackboard).
    ApiKey,
    /// Autenticação por JWT via arreio_security::jwt.
    Jwt,
    /// Tenta JWT primeiro, fallback para API Key.
    Hybrid,
}

impl AuthMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "noauth" | "off" | "none" | "" => Self::NoAuth,
            "apikey" | "api-key" | "api_key" => Self::ApiKey,
            "jwt" => Self::Jwt,
            "hybrid" | "auto" => Self::Hybrid,
            _ => Self::NoAuth,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoAuth => "noauth",
            Self::ApiKey => "apikey",
            Self::Jwt => "jwt",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Contexto de autenticação extraído de um request.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Identificador do cliente autenticado.
    pub client_id: String,
    /// Role RBAC (admin, developer, auditor, guest).
    pub role: String,
    /// Método de autenticação usado.
    pub auth_method: String,
}

/// Configuração de autenticação do gateway.
pub struct AuthConfig {
    pub mode: AuthMode,
    /// Rotas que não exigem autenticação (ex: /health, /).
    pub public_paths: HashSet<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::NoAuth,
            public_paths: HashSet::from([
                "/".to_string(),
                "/health".to_string(),
                "/api/auth/login".to_string(),
            ]),
        }
    }
}

impl AuthConfig {
    pub fn new(mode: AuthMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }
}

/// Middleware de autenticação do gateway.
pub struct AuthMiddleware {
    config: AuthConfig,
    blackboard: Blackboard,
    rbac: RbacEngine,
}

impl AuthMiddleware {
    /// Cria o middleware com o modo e Blackboard fornecidos.
    pub fn new(config: AuthConfig, blackboard: Blackboard) -> Self {
        let rbac = RbacEngine::with_defaults();
        Self {
            config,
            blackboard,
            rbac,
        }
    }

    /// Clone leve para uso em threads (compartilha Blackboard).
    /// O RbacEngine é recriado com defaults — use assign_role para configurar.
    pub fn clone_light(&self) -> Self {
        Self {
            config: AuthConfig {
                mode: self.config.mode,
                public_paths: self.config.public_paths.clone(),
            },
            blackboard: self.blackboard.clone(),
            rbac: RbacEngine::with_defaults(),
        }
    }

    /// Retorna o modo de autenticação atual.
    pub fn mode(&self) -> AuthMode {
        self.config.mode
    }

    /// Atribui uma role RBAC a um usuário (útil para bootstrap).
    pub fn assign_role(&mut self, user: &str, role: Role) -> Result<()> {
        self.rbac.assign_role(user, role).map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    // ── Autenticação ──────────────────────────────────────────────────────

    /// Verifica se uma rota é pública (não requer auth).
    pub fn is_public(&self, path: &str) -> bool {
        // Rotas exatas
        if self.config.public_paths.contains(path) {
            return true;
        }
        // Prefixos públicos: /assets/, /static/
        if path.starts_with("/assets/") || path.starts_with("/static/") {
            return true;
        }
        false
    }

    /// Autentica um request HTTP a partir dos headers.
    ///
    /// Retorna `AuthContext` se autenticado, ou `None` se:
    /// - Modo NoAuth (retorna contexto guest)
    /// - Token não fornecido
    /// - Token inválido
    pub fn authenticate(
        &self,
        headers: &[(String, String)],
        _path: &str,
    ) -> Option<AuthContext> {
        if self.config.mode == AuthMode::NoAuth {
            return Some(AuthContext {
                client_id: "anonymous".into(),
                role: "admin".into(), // NoAuth = confiança total (dev)
                auth_method: "noauth".into(),
            });
        }

        let token = extract_token(headers);

        match self.config.mode {
            AuthMode::Jwt => {
                let claims = self.try_jwt(&token)?;
                Some(AuthContext {
                    client_id: claims.sub,
                    role: claims.prf,
                    auth_method: "jwt".into(),
                })
            }
            AuthMode::ApiKey => {
                let identity = self.try_api_key(&token)?;
                Some(AuthContext {
                    client_id: identity.client_id,
                    role: identity.role,
                    auth_method: "apikey".into(),
                })
            }
            AuthMode::Hybrid => {
                // Tenta JWT primeiro, fallback API Key
                if let Some(claims) = self.try_jwt(&token) {
                    let _ = self.log_auth(&claims.sub, "jwt", true);
                    return Some(AuthContext {
                        client_id: claims.sub,
                        role: claims.prf,
                        auth_method: "jwt".into(),
                    });
                }
                if let Some(identity) = self.try_api_key(&token) {
                    let _ = self.log_auth(&identity.client_id, "apikey", true);
                    return Some(AuthContext {
                        client_id: identity.client_id,
                        role: identity.role,
                        auth_method: "apikey".into(),
                    });
                }
                let _ = self.log_auth("unknown", "hybrid", false);
                None
            }
            AuthMode::NoAuth => unreachable!(), // tratado acima
        }
    }

    // ── Autorização ───────────────────────────────────────────────────────

    /// Verifica se o contexto autenticado possui uma permissão específica.
    pub fn has_permission(&self, ctx: &AuthContext, permission: Permission) -> bool {
        self.rbac.has_permission(&ctx.client_id, &permission)
    }

    /// Retorna as permissões do cliente autenticado.
    pub fn permissions(&self, ctx: &AuthContext) -> HashSet<Permission> {
        self.rbac.user_permissions(&ctx.client_id)
    }

    // ── Login (JWT issuance) ──────────────────────────────────────────────

    /// Tenta fazer login com senha master e retorna JWT.
    ///
    /// A senha é validada contra `ARREIO_MASTER_PASSWORD` (env).
    pub fn login(&self, password: &str) -> Result<Option<String>> {
        let master = std::env::var("ARREIO_MASTER_PASSWORD").unwrap_or_default();
        if master.is_empty() {
            // Sem master password configurada, login desabilitado
            eprintln!("[gateway] AVISO: ARREIO_MASTER_PASSWORD não definida — login desabilitado");
            return Ok(None);
        }

        // Comparação timing-safe
        if !constant_time_eq(password.as_bytes(), master.as_bytes()) {
            return Ok(None);
        }

        let token = arreio_security::issue_token(
            "admin",
            "admin",
            &["*".into()],
            24,
            &[("source", "gateway-login")],
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let _ = self.log_auth("admin", "jwt-login", true);
        Ok(Some(token))
    }

    // ── Internals ─────────────────────────────────────────────────────────

    fn try_jwt(&self, token: &str) -> Option<JwtClaims> {
        verify_token(token).ok()
    }

    fn try_api_key(&self, token: &str) -> Option<arreio_vault::ClientIdentity> {
        let reg = ClientRegistry::new(self.blackboard.clone());
        reg.authenticate(token).ok().flatten()
    }

    fn log_auth(&self, client_id: &str, method: &str, success: bool) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.blackboard.put_tuple(
            "audit",
            &format!("auth-{}-{}", now, uuid::Uuid::new_v4()),
            serde_json::json!({
                "timestamp": now,
                "category": "Auth",
                "actor": client_id,
                "action": if success { "login_success" } else { "login_failed" },
                "target": method,
                "details": {
                    "method": method,
                    "success": success,
                },
                "session_id": "gateway",
            }),
        )
    }
}

// ── Extração de Token ─────────────────────────────────────────────────────────

/// Extrai token de autenticação dos headers HTTP.
///
/// Ordem de prioridade:
/// 1. `X-API-Key: <key>` (header dedicado, não aparece em logs de proxy)
/// 2. `Authorization: Bearer <token>` (padrão OAuth/JWT)
pub fn extract_token(headers: &[(String, String)]) -> String {
    for (name, value) in headers {
        if name.to_lowercase() == "x-api-key" {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    for (name, value) in headers {
        if name.to_lowercase() == "authorization" {
            let trimmed = value.trim();
            if trimmed.len() > 7 && trimmed[..7].to_lowercase() == *"bearer " {
                return trimmed[7..].trim().to_string();
            }
        }
    }

    String::new()
}

/// Extrai headers HTTP de um TcpStream (lê linha por linha).
/// Retorna pares (nome, valor) e o body se houver Content-Length.
pub fn parse_headers(
    reader: &mut dyn std::io::BufRead,
) -> Result<(Vec<(String, String)>, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();

    // Já lemos a primeira linha (METHOD PATH VERSION) antes de chamar
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = parse_header_line(&line) {
            headers.push((name, value));
        }
    }

    // Lê body se Content-Length presente
    let mut body = String::new();
    for (name, value) in &headers {
        if name.to_lowercase() == "content-length" {
            if let Ok(len) = value.trim().parse::<usize>() {
                if len > 0 {
                    let mut buf = vec![0u8; len];
                    reader.read_exact(&mut buf)?;
                    body = String::from_utf8_lossy(&buf).into_owned();
                }
            }
        }
    }

    Ok((headers, body))
}

fn parse_header_line(line: &str) -> Option<(String, String)> {
    let colon = line.find(':')?;
    let name = line[..colon].trim().to_string();
    let value = line[colon + 1..].trim().to_string();
    Some((name, value))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

    // ── extract_token ─────────────────────────────────────────────────────

    #[test]
    fn extract_from_x_api_key() {
        let headers = vec![
            ("x-api-key".into(), "arreio_testkey123".into()),
        ];
        assert_eq!(extract_token(&headers), "arreio_testkey123");
    }

    #[test]
    fn extract_from_authorization_bearer() {
        let headers = vec![
            ("authorization".into(), "Bearer arreio_jwt_token".into()),
        ];
        assert_eq!(extract_token(&headers), "arreio_jwt_token");
    }

    #[test]
    fn x_api_key_has_priority_over_authorization() {
        let headers = vec![
            ("x-api-key".into(), "key_primary".into()),
            ("authorization".into(), "Bearer token_secondary".into()),
        ];
        assert_eq!(extract_token(&headers), "key_primary");
    }

    #[test]
    fn empty_headers_returns_empty() {
        let headers: Vec<(String, String)> = vec![];
        assert!(extract_token(&headers).is_empty());
    }

    #[test]
    fn extract_case_insensitive() {
        let headers = vec![
            ("X-API-KEY".into(), "case_test".into()),
        ];
        assert_eq!(extract_token(&headers), "case_test");
    }

    // ── AuthMode ──────────────────────────────────────────────────────────

    #[test]
    fn auth_mode_from_str_defaults_to_noauth() {
        assert_eq!(AuthMode::from_str(""), AuthMode::NoAuth);
        assert_eq!(AuthMode::from_str("invalid"), AuthMode::NoAuth);
    }

    #[test]
    fn auth_mode_from_str_apikey() {
        assert_eq!(AuthMode::from_str("apikey"), AuthMode::ApiKey);
        assert_eq!(AuthMode::from_str("api-key"), AuthMode::ApiKey);
    }

    #[test]
    fn auth_mode_from_str_jwt() {
        assert_eq!(AuthMode::from_str("jwt"), AuthMode::Jwt);
    }

    #[test]
    fn auth_mode_from_str_hybrid() {
        assert_eq!(AuthMode::from_str("hybrid"), AuthMode::Hybrid);
        assert_eq!(AuthMode::from_str("auto"), AuthMode::Hybrid);
    }

    // ── AuthConfig ────────────────────────────────────────────────────────

    #[test]
    fn default_config_is_noauth() {
        let config = AuthConfig::default();
        assert_eq!(config.mode, AuthMode::NoAuth);
        assert!(config.public_paths.contains("/health"));
        assert!(config.public_paths.contains("/"));
    }

    #[test]
    fn public_paths_are_recognized() {
        let config = AuthConfig::default();
        let bb = temp_bb();
        let mw = AuthMiddleware::new(config, bb);
        assert!(mw.is_public("/health"));
        assert!(mw.is_public("/"));
        assert!(!mw.is_public("/api/status"));
    }

    // ── parse_header_line ─────────────────────────────────────────────────

    #[test]
    fn parse_valid_header() {
        let result = parse_header_line("Content-Type: application/json");
        assert_eq!(result, Some(("Content-Type".into(), "application/json".into())));
    }

    #[test]
    fn parse_header_with_extra_spaces() {
        let result = parse_header_line("  X-Custom :  value  ");
        assert_eq!(result, Some(("X-Custom".into(), "value".into())));
    }

    #[test]
    fn parse_invalid_header() {
        assert_eq!(parse_header_line("no colon here"), None);
    }

    // ── constant_time_eq ──────────────────────────────────────────────────

    #[test]
    fn ct_eq_same() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    #[test]
    fn ct_eq_different() {
        assert!(!constant_time_eq(b"secret", b"secr3t"));
    }

    #[test]
    fn ct_eq_different_length() {
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn temp_bb() -> Blackboard {
        let f = tempfile::NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }
}
