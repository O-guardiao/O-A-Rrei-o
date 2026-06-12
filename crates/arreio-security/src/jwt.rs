//! JWT (JSON Web Token) — Camada 2 de Autenticação (OpenClaw ADR-003).
//!
//! Implementação pura de JWT com HMAC-SHA256. Zero dependências externas
//! além de `sha2` (já no workspace). Não conhece HTTP, Blackboard, ou rede.
//!
//! ## Formato do token
//! `base64url(header).base64url(payload).base64url(HMAC-SHA256(header.payload))`
//!
//! ## Propriedades de segurança
//! - HMAC-SHA256 com comparação timing-safe (`constant_time_eq`)
//! - Expiração obrigatória (`exp` claim)
//! - Sem suporte a `alg=none`
//! - Secret lido de env `ARREIO_JWT_SECRET` (mínimo 32 caracteres)
//!
//! ## Claims padrão
//! - `sub`: identificador do cliente (client_id)
//! - `prf`: profile/role (admin, developer, auditor, guest)
//! - `prm`: permissions (lista de strings)
//! - `exp`: expiração (Unix timestamp)
//! - `iat`: emitido em (Unix timestamp)
//! - `jti`: JWT ID único (prevenção de replay)

use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Constantes ────────────────────────────────────────────────────────────────

#[allow(dead_code)]
const ALGORITHM: &str = "HS256";
const HEADER_B64: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"; // {"alg":"HS256","typ":"JWT"}
const MIN_SECRET_LENGTH: usize = 32;
#[allow(dead_code)]
const DEFAULT_TTL_HOURS: u64 = 24;

// ── Erros ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    MissingSecret,
    SecretTooShort,
    MalformedToken,
    InvalidSignature,
    TokenExpired,
    InvalidClaim,
    Utf8Error,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSecret => write!(f, "ARREIO_JWT_SECRET não configurada"),
            Self::SecretTooShort => write!(
                f,
                "ARREIO_JWT_SECRET deve ter no mínimo {} caracteres",
                MIN_SECRET_LENGTH
            ),
            Self::MalformedToken => write!(f, "token malformado (não é JWT válido)"),
            Self::InvalidSignature => write!(f, "assinatura JWT inválida"),
            Self::TokenExpired => write!(f, "token expirado"),
            Self::InvalidClaim => write!(f, "claim inválida ou ausente"),
            Self::Utf8Error => write!(f, "erro de decodificação UTF-8 no payload"),
        }
    }
}

impl std::error::Error for JwtError {}

// ── Claims ────────────────────────────────────────────────────────────────────

/// Claims extraíveis de um token JWT verificado.
#[derive(Debug, Clone)]
pub struct JwtClaims {
    /// Subject — identificador do cliente.
    pub sub: String,
    /// Profile / role (admin, developer, auditor, guest).
    pub prf: String,
    /// Lista de permissões.
    pub prm: Vec<String>,
    /// Timestamp de expiração (Unix).
    pub exp: u64,
    /// Timestamp de emissão (Unix).
    pub iat: u64,
    /// JWT ID único.
    pub jti: String,
}

// ── Helpers internos ──────────────────────────────────────────────────────────

/// Base64URL encode sem padding.
fn b64url_encode(data: &[u8]) -> String {
    // Base64 padrão, depois converte para URL-safe e remove padding
    let b64 = base64_encode(data);
    b64.replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
}

/// Base64 padrão (para uso interno).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Base64URL decode com restauração de padding.
fn b64url_decode(input: &str) -> Result<Vec<u8>, JwtError> {
    let mut s = input.replace('-', "+").replace('_', "/");
    // Restaura padding
    let padding = 4 - (s.len() % 4);
    if padding != 4 {
        s.push_str(&"=".repeat(padding));
    }
    base64_decode(&s)
}

/// Base64 decode padrão.
fn base64_decode(input: &str) -> Result<Vec<u8>, JwtError> {
    const DECODE: [i8; 128] = {
        let mut table = [-1i8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < chars.len() {
            table[chars[i] as usize] = i as i8;
            i += 1;
        }
        table
    };

    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' || bytes[i] == b'\n' || bytes[i] == b'\r' {
            break;
        }
        let b0 = DECODE.get(bytes[i] as usize).copied().unwrap_or(-1);
        let b1 = DECODE.get(bytes.get(i + 1).copied().unwrap_or(b'A') as usize).copied().unwrap_or(-1);
        let b2 = DECODE.get(bytes.get(i + 2).copied().unwrap_or(b'A') as usize).copied().unwrap_or(-1);
        let b3 = DECODE.get(bytes.get(i + 3).copied().unwrap_or(b'A') as usize).copied().unwrap_or(-1);

        if b0 < 0 || b1 < 0 {
            return Err(JwtError::MalformedToken);
        }
        out.push(((b0 as u32) << 2 | (b1 as u32) >> 4) as u8);
        if b2 >= 0 {
            out.push(((b1 as u32) << 4 | (b2 as u32) >> 2) as u8);
        }
        if b3 >= 0 {
            out.push(((b2 as u32) << 6 | b3 as u32) as u8);
        }
        i += 4;
    }
    Ok(out)
}

/// HMAC-SHA256 usando `sha2` nativo (sem crate `hmac` externo).
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK_SIZE: usize = 64;
    let mut key_padded = [0u8; BLOCK_SIZE];

    if key.len() > BLOCK_SIZE {
        let h = Sha256::digest(key);
        key_padded[..h.len()].copy_from_slice(&h);
    } else {
        key_padded[..key.len()].copy_from_slice(key);
    }

    let mut o_key_pad = [0u8; BLOCK_SIZE];
    let mut i_key_pad = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        o_key_pad[i] = key_padded[i] ^ 0x5c;
        i_key_pad[i] = key_padded[i] ^ 0x36;
    }

    let mut inner = Sha256::new();
    inner.update(&i_key_pad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&o_key_pad);
    outer.update(&inner_hash);
    outer.finalize().to_vec()
}

/// Comparação timing-safe para tokens.
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

/// SHA-256 de string (para hash de API keys).
pub fn hash_token(raw: &str) -> String {
    let h = Sha256::digest(raw.as_bytes());
    hex_encode(&h)
}

fn hex_encode(data: &[u8]) -> String {
    let hex: Vec<u8> = data.iter().flat_map(|b| {
        let hi = (b >> 4) & 0x0F;
        let lo = b & 0x0F;
        [hex_char(hi), hex_char(lo)]
    }).collect();
    String::from_utf8(hex).unwrap_or_default()
}

fn hex_char(n: u8) -> u8 {
    match n {
        0..=9 => b'0' + n,
        _ => b'a' + (n - 10),
    }
}

#[allow(dead_code)]
fn hex_decode(hex: &str) -> Result<Vec<u8>, JwtError> {
    if hex.len() % 2 != 0 {
        return Err(JwtError::MalformedToken);
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| {
            let hi = hex_digit(hex.as_bytes()[i]);
            let lo = hex_digit(hex.as_bytes()[i + 1]);
            (hi << 4) | lo
        })
        .collect();
    Ok(bytes)
}

#[allow(dead_code)]
fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Unix timestamp atual em segundos.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── API Pública ───────────────────────────────────────────────────────────────

/// Emite um JWT assinado para um cliente.
///
/// Lê o secret de `ARREIO_JWT_SECRET`. Use `issue_token_with_secret` se
/// já possui o secret.
pub fn issue_token(
    client_id: &str,
    profile: &str,
    permissions: &[String],
    ttl_hours: u64,
    extra_claims: &[(&str, &str)],
) -> Result<String, JwtError> {
    let secret = get_secret()?;
    issue_token_with_secret(client_id, profile, permissions, ttl_hours, extra_claims, &secret)
}

/// Emite um JWT assinado com secret explícito.
pub fn issue_token_with_secret(
    client_id: &str,
    profile: &str,
    permissions: &[String],
    ttl_hours: u64,
    extra_claims: &[(&str, &str)],
    secret: &str,
) -> Result<String, JwtError> {
    if secret.len() < MIN_SECRET_LENGTH {
        return Err(JwtError::SecretTooShort);
    }
    let now = now_epoch();
    let exp = now + (ttl_hours * 3600);
    let jti = uuid::Uuid::new_v4().to_string();

    let prm_json = serde_json::to_string(permissions).unwrap_or_else(|_| "[]".to_string());

    let mut payload_parts: Vec<String> = vec![
        format!(r#""sub":"{}""#, escape_json(client_id)),
        format!(r#""prf":"{}""#, escape_json(profile)),
        format!(r#""prm":{}"#, prm_json),
        format!(r#""exp":{}"#, exp),
        format!(r#""iat":{}"#, now),
        format!(r#""jti":"{}""#, jti),
    ];

    for (k, v) in extra_claims {
        payload_parts.push(format!(r#""{}":"{}""#, escape_json(k), escape_json(v)));
    }

    let payload_json = format!("{{{}}}", payload_parts.join(","));
    let payload_b64 = b64url_encode(payload_json.as_bytes());

    let header_payload = format!("{}.{}", HEADER_B64, payload_b64);
    let sig = hmac_sha256(secret.as_bytes(), header_payload.as_bytes());
    let sig_b64 = b64url_encode(&sig);

    Ok(format!("{}.{}", header_payload, sig_b64))
}

/// Verifica um token JWT e extrai as claims.
///
/// Lê o secret de `ARREIO_JWT_SECRET`. Use `verify_token_with_secret` se
/// já possui o secret.
pub fn verify_token(token: &str) -> Result<JwtClaims, JwtError> {
    let secret = get_secret()?;
    verify_token_with_secret(token, &secret)
}

/// Verifica um token JWT com secret explícito e extrai as claims.
pub fn verify_token_with_secret(token: &str, secret: &str) -> Result<JwtClaims, JwtError> {
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(JwtError::MalformedToken);
    }

    let header_b64 = parts[0];
    let payload_b64 = parts[1];
    let sig_b64 = parts[2];

    // Verificar assinatura
    let header_payload = format!("{}.{}", header_b64, payload_b64);
    let expected_sig = hmac_sha256(secret.as_bytes(), header_payload.as_bytes());
    let expected_sig_b64 = b64url_encode(&expected_sig);

    // Decodificar assinatura recebida para comparação timing-safe
    let received_sig = b64url_decode(sig_b64)?;
    let expected_sig_raw = b64url_decode(&expected_sig_b64)?;

    if !constant_time_eq(&received_sig, &expected_sig_raw) {
        return Err(JwtError::InvalidSignature);
    }

    // Decodificar payload
    let payload_bytes = b64url_decode(payload_b64)?;
    let payload_str =
        String::from_utf8(payload_bytes).map_err(|_| JwtError::Utf8Error)?;

    let claims: serde_json::Value =
        serde_json::from_str(&payload_str).map_err(|_| JwtError::InvalidClaim)?;

    let exp = claims
        .get("exp")
        .and_then(|v| v.as_u64())
        .ok_or(JwtError::InvalidClaim)?;

    if exp < now_epoch() {
        return Err(JwtError::TokenExpired);
    }

    let sub = claims
        .get("sub")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let prf = claims
        .get("prf")
        .and_then(|v| v.as_str())
        .unwrap_or("guest")
        .to_string();
    let prm: Vec<String> = claims
        .get("prm")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let iat = claims.get("iat").and_then(|v| v.as_u64()).unwrap_or(0);
    let jti = claims
        .get("jti")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(JwtClaims {
        sub,
        prf,
        prm,
        exp,
        iat,
        jti,
    })
}

// ── Gerenciamento de Secret ───────────────────────────────────────────────────

fn get_secret() -> Result<String, JwtError> {
    let secret = std::env::var("ARREIO_JWT_SECRET").unwrap_or_default();
    if secret.is_empty() {
        return Err(JwtError::MissingSecret);
    }
    if secret.len() < MIN_SECRET_LENGTH {
        return Err(JwtError::SecretTooShort);
    }
    Ok(secret)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "a-very-secure-secret-key-for-testing-32chars";

    // ── Base64 ────────────────────────────────────────────────────────────

    #[test]
    fn base64_roundtrip() {
        let data = b"hello world";
        let enc = b64url_encode(data);
        let dec = b64url_decode(&enc).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn base64_decode_with_padding() {
        let dec = b64url_decode("aGVsbG8").unwrap();
        assert_eq!(dec, b"hello");
    }

    // ── HMAC ──────────────────────────────────────────────────────────────

    #[test]
    fn hmac_sha256_known_vector() {
        let key = vec![0x0bu8; 20];
        let data = b"Hi There";
        let mac = hmac_sha256(&key, data);
        let expected = hex_decode("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7").unwrap();
        assert_eq!(hex_encode(&mac), hex_encode(&expected));
    }

    // ── JWT ───────────────────────────────────────────────────────────────

    #[test]
    fn issue_and_verify_roundtrip() {
        let token = issue_token_with_secret(
            "client-1", "developer", &["read".into(), "write".into()], 24, &[], TEST_SECRET
        ).unwrap();
        let claims = verify_token_with_secret(&token, TEST_SECRET).unwrap();
        assert_eq!(claims.sub, "client-1");
        assert_eq!(claims.prf, "developer");
        assert_eq!(claims.prm, vec!["read", "write"]);
    }

    #[test]
    fn verify_tampered_token_fails() {
        let token = issue_token_with_secret(
            "client-1", "developer", &[], 24, &[], TEST_SECRET
        ).unwrap();
        let mut parts: Vec<&str> = token.splitn(3, '.').collect();
        parts[1] = "eyJzdWIiOiJoYWNrZXIifQ";
        let tampered = parts.join(".");
        assert!(verify_token_with_secret(&tampered, TEST_SECRET).is_err());
    }

    #[test]
    fn verify_expired_token_fails() {
        let token = issue_token_with_secret(
            "client-1", "guest", &[], 0, &[], TEST_SECRET
        ).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        match verify_token_with_secret(&token, TEST_SECRET) {
            Err(JwtError::TokenExpired) => {}
            other => panic!("esperado TokenExpired, obtido {:?}", other),
        }
    }

    #[test]
    fn verify_malformed_token_fails() {
        assert!(verify_token_with_secret("not.a.jwt.token", TEST_SECRET).is_err());
        assert!(verify_token_with_secret("only.twoparts", TEST_SECRET).is_err());
    }

    #[test]
    fn short_secret_rejected() {
        match issue_token_with_secret("c", "guest", &[], 24, &[], "curta") {
            Err(JwtError::SecretTooShort) => {}
            other => panic!("esperado SecretTooShort, obtido {:?}", other),
        }
    }

    #[test]
    fn missing_secret_from_env() {
        std::env::remove_var("ARREIO_JWT_SECRET");
        match issue_token("c", "guest", &[], 24, &[]) {
            Err(JwtError::MissingSecret) => {}
            other => panic!("esperado MissingSecret, obtido {:?}", other),
        }
    }

    #[test]
    fn short_secret_from_env() {
        std::env::set_var("ARREIO_JWT_SECRET", "curta");
        match issue_token("c", "guest", &[], 24, &[]) {
            Err(JwtError::SecretTooShort) => {}
            other => panic!("esperado SecretTooShort, obtido {:?}", other),
        }
    }

    #[test]
    fn hash_token_deterministic() {
        let h1 = hash_token("my-secret-key");
        let h2 = hash_token("my-secret-key");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_token_different_keys() {
        let h1 = hash_token("key-1");
        let h2 = hash_token("key-2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn constant_time_eq_true() {
        assert!(constant_time_eq(b"abc", b"abc"));
    }

    #[test]
    fn constant_time_eq_false() {
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn extra_claims_present() {
        let token = issue_token_with_secret(
            "c1", "admin", &[], 24, &[("org", "arreio")], TEST_SECRET
        ).unwrap();
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        let payload_bytes = b64url_decode(parts[1]).unwrap();
        let payload_str = String::from_utf8(payload_bytes).unwrap();
        assert!(payload_str.contains(r#""org":"arreio""#));
    }
}
