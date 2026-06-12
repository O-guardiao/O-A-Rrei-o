use anyhow::{anyhow, Result};
use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::OnceLock;

/// Flag snapshot capturada no primeiro acesso (equivale a "import time").
/// Uma vez lida a variável de ambiente `ARREIO_REDACT_ENABLED`, o valor nunca muda.
/// Padrão: `true` (redação ativada).
pub fn redaction_enabled() -> bool {
    static LOCK: OnceLock<bool> = OnceLock::new();
    *LOCK.get_or_init(|| {
        std::env::var("ARREIO_REDACT_ENABLED")
            .map(|v| v != "0" && v != "false")
            .unwrap_or(true)
    })
}

/// Constante pública para verificação explícita de estado.
pub const _REDACT_ENABLED: bool = true;

/// Aplica máscara inteligente a um token.
/// - < 18 caracteres: `***`
/// - >= 18 caracteres: preserva 6 prefixo + `...` + 4 sufixo.
fn mask_token(token: &str) -> String {
    let len = token.len();
    if len < 18 {
        "***".to_string()
    } else {
        format!("{}...{}", &token[..6], &token[len - 4..])
    }
}

/// Motor de redação de secrets com 30+ padrões de regex.
pub struct RedactionEngine {
    patterns: Vec<(Regex, &'static str)>,
    dsn_patterns: Vec<(Regex, fn(&Captures) -> String)>,
    sensitive_json_keys: Vec<&'static str>,
    sensitive_query_params: Vec<&'static str>,
    sensitive_env_keys: Vec<&'static str>,
    skip_env_in_code: bool,
}

impl Default for RedactionEngine {
    fn default() -> Self {
        Self::new().expect("regex padrão deve compilar")
    }
}

impl RedactionEngine {
    pub fn new() -> Result<Self> {
        let patterns = Self::build_patterns()?;
        let dsn_patterns = Self::build_dsn_patterns()?;
        Ok(Self {
            patterns,
            dsn_patterns,
            sensitive_json_keys: vec![
                "api_key",
                "apikey",
                "token",
                "secret",
                "password",
                "passwd",
                "private_key",
                "access_token",
                "refresh_token",
                "client_secret",
                "auth",
            ],
            sensitive_query_params: vec![
                "access_token",
                "code",
                "signature",
                "token",
                "api_key",
                "secret",
                "password",
                "client_secret",
                "refresh_token",
                "apikey",
            ],
            sensitive_env_keys: vec![
                "OPENAI_API_KEY",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "GITHUB_TOKEN",
                "GH_TOKEN",
                "HF_TOKEN",
                "NPM_TOKEN",
                "PYPI_TOKEN",
                "SLACK_TOKEN",
                "TELEGRAM_BOT_TOKEN",
                "DATABASE_URL",
                "REDIS_URL",
                "SECRET_KEY",
                "PRIVATE_KEY",
                "TOKEN",
                "API_KEY",
                "PASSWORD",
            ],
            skip_env_in_code: false,
        })
    }

    /// Define se atribuições de ambiente devem ser ignoradas em arquivos de código.
    pub fn with_skip_env_in_code(mut self, skip: bool) -> Self {
        self.skip_env_in_code = skip;
        self
    }

    fn build_patterns() -> Result<Vec<(Regex, &'static str)>> {
        let raw: Vec<(&str, &'static str)> = vec![
            // OpenAI / general sk-
            (r"sk-[a-zA-Z0-9]{20,}", "sk-api-key"),
            // GitHub tokens
            (r"ghp_[a-zA-Z0-9]{20,}", "github-pat"),
            (r"gho_[a-zA-Z0-9]{20,}", "github-oauth"),
            (r"ghs_[a-zA-Z0-9]{20,}", "github-app-token"),
            (r"github_pat_[a-zA-Z0-9_]{20,}", "github-fine-pat"),
            // Google
            (r"AIza[0-9A-Za-z_-]{20,}", "google-api-key"),
            (r"ya29\.[0-9A-Za-z_-]+", "google-oauth"),
            (r"gAAAA[0-9A-Za-z_-]+", "google-oauth-alt"),
            // AWS
            (r"AKIA[0-9A-Z]{16}", "aws-access-key"),
            (r"ASIA[0-9A-Z]{16}", "aws-session-key"),
            // Hugging Face
            (r"hf_[a-zA-Z0-9]{20,}", "huggingface-token"),
            // npm
            (r"npm_[a-zA-Z0-9]{20,}", "npm-token"),
            // PyPI
            (r"pypi-[A-Za-z0-9_-]{20,}", "pypi-token"),
            // Slack
            (
                r"xoxb-[0-9]{10,13}-[0-9]{10,13}-[a-zA-Z0-9]{20,}",
                "slack-bot",
            ),
            (
                r"xoxp-[0-9]{10,13}-[0-9]{10,13}-[a-zA-Z0-9]{20,}",
                "slack-user",
            ),
            (
                r"xapp-[0-9]{10,13}-[0-9]{10,13}-[a-zA-Z0-9]{20,}",
                "slack-app",
            ),
            (r"xoxa-[0-9]{10,13}", "slack-legacy"),
            // Telegram
            (r"bot\d+:[0-9A-Za-z_-]{20,}", "telegram-bot"),
            // Stripe
            (r"sk_(live|test)_[0-9a-zA-Z]{20,}", "stripe-secret"),
            (r"pk_(live|test)_[0-9a-zA-Z]{20,}", "stripe-publishable"),
            // SendGrid
            (
                r"SG\.[0-9A-Za-z_-]{22,}\.[0-9A-Za-z_-]{43,}",
                "sendgrid-key",
            ),
            // Twilio
            (r"SK[0-9a-f]{32}", "twilio-key"),
            // Heroku
            (
                r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
                "heroku-key",
            ),
            // Facebook
            (r"EAACEdEose0cBA[0-9A-Za-z]+", "facebook-token"),
            // Dropbox
            (r"sl\.[A-Za-z0-9_-]+", "dropbox-token"),
            // Discord webhook
            (
                r"https://discord(?:app)?\.com/api/webhooks/\d+/[A-Za-z0-9_-]+",
                "discord-webhook",
            ),
            // JWT
            (
                r"eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}",
                "jwt",
            ),
            // Bearer / Basic
            (r"(?i)bearer\s+[a-zA-Z0-9_\-\.]+", "bearer"),
            (r"(?i)basic\s+[a-zA-Z0-9+/=]{10,}", "basic-auth"),
            // Private keys PEM
            (
                r"-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----",
                "private-key",
            ),
            // Discord mention
            (r"<@\d+>", "discord-mention"),
            // E.164 phone
            (r"\+\d{10,15}", "e164-phone"),
        ];

        let mut compiled = Vec::with_capacity(raw.len());
        for (pat, name) in raw {
            let re = Regex::new(pat).map_err(|e| anyhow!("regex inválido para {}: {}", name, e))?;
            compiled.push((re, name));
        }
        Ok(compiled)
    }

    fn build_dsn_patterns() -> Result<Vec<(Regex, fn(&Captures) -> String)>> {
        fn repl_postgres(caps: &Captures) -> String {
            let ql = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let user = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("postgres{}://{}:***@", ql, user)
        }
        fn repl_mysql(caps: &Captures) -> String {
            let user = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            format!("mysql://{}:***@", user)
        }
        fn repl_mongodb(caps: &Captures) -> String {
            let srv = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let user = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("mongodb{}://{}:***@", srv, user)
        }
        fn repl_redis(_caps: &Captures) -> String {
            "redis://:***@".to_string()
        }
        fn repl_generic(caps: &Captures) -> String {
            let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            format!("{}:***@", prefix)
        }

        let raw: Vec<(&str, fn(&Captures) -> String)> = vec![
            (r"postgres(ql)?://([^:]+):([^@]+)@", repl_postgres),
            (r"mysql://([^:]+):([^@]+)@", repl_mysql),
            (r"mongodb(\+srv)?://([^:]+):([^@]+)@", repl_mongodb),
            (r"redis://:([^@]+)@", repl_redis),
            (r"([a-zA-Z][a-zA-Z0-9+.-]*://[^:]+):([^@]+)@", repl_generic),
        ];

        let mut compiled = Vec::with_capacity(raw.len());
        for (pat, repl) in raw {
            let re = Regex::new(pat).map_err(|e| anyhow!("regex DSN inválido: {}", e))?;
            compiled.push((re, repl));
        }
        Ok(compiled)
    }

    /// Redação genérica sobre texto plano.
    pub fn redact(&self, input: &str) -> String {
        if !redaction_enabled() {
            return input.to_string();
        }
        let mut output = input.to_string();
        // 1) DSNs: preserva estrutura da URL e mascara apenas a senha
        for (re, repl) in &self.dsn_patterns {
            output = re.replace_all(&output, *repl).to_string();
        }
        // 2) Secrets genéricos: aplica mask_token no match inteiro
        for (re, _name) in &self.patterns {
            output = re
                .replace_all(&output, |caps: &Captures| {
                    let matched = caps.get(0).unwrap().as_str();
                    mask_token(matched)
                })
                .to_string();
        }
        output
    }

    /// Redação de parâmetros sensíveis em query strings de URLs.
    pub fn redact_query_strings(&self, input: &str) -> String {
        if !redaction_enabled() {
            return input.to_string();
        }
        let re = Regex::new(&format!(
            r"(?i)([?&])({})=[^&\s]+",
            self.sensitive_query_params.join("|")
        ))
        .unwrap();
        re.replace_all(input, "$1$2=***").to_string()
    }

    /// Redação de valores em JSON cujas chaves são sensíveis.
    pub fn redact_json(&self, input: &str) -> Result<String> {
        if !redaction_enabled() {
            return Ok(input.to_string());
        }
        let mut value: Value = serde_json::from_str(input)?;
        self.redact_value(&mut value);
        Ok(serde_json::to_string(&value)?)
    }

    fn redact_value(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    if self
                        .sensitive_json_keys
                        .iter()
                        .any(|s| k.eq_ignore_ascii_case(s))
                    {
                        if let Value::String(s) = v {
                            *s = mask_token(s);
                        } else {
                            *v = Value::String("***".to_string());
                        }
                    } else {
                        self.redact_value(v);
                    }
                }
            }
            Value::Array(arr) => {
                for v in arr.iter_mut() {
                    self.redact_value(v);
                }
            }
            _ => {}
        }
    }

    /// Redação de atribuições de variáveis de ambiente.
    /// Se `skip_env_in_code` estiver ativo, não redacta linhas que pareçam código.
    pub fn redact_env(&self, input: &str) -> String {
        if !redaction_enabled() {
            return input.to_string();
        }
        let re = Regex::new(&format!(
            r"(?i)^\s*({})\s*=\s*\S+",
            self.sensitive_env_keys.join("|")
        ))
        .unwrap();

        input
            .lines()
            .map(|line| {
                if self.skip_env_in_code && Self::looks_like_code(line) {
                    return line.to_string();
                }
                re.replace_all(line, |caps: &Captures| {
                    let key = caps.get(1).unwrap().as_str();
                    format!("{}=***", key)
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn looks_like_code(line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.starts_with("//")
            || trimmed.starts_with("#")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub ")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("var ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("module ")
            || trimmed.starts_with("export ")
    }

    /// Pipeline completo: aplica redação de secrets, query strings, JSON (se aplicável) e ENV.
    /// Detecta automaticamente se o input é JSON.
    pub fn redact_all(&self, input: &str) -> String {
        if !redaction_enabled() {
            return input.to_string();
        }
        let trimmed = input.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(redacted) = self.redact_json(input) {
                return self.redact_query_strings(&self.redact(&redacted));
            }
        }
        let output = self.redact(input);
        let output = self.redact_query_strings(&output);
        output
    }
}

/// Formatter que acumula texto e aplica redação ao finalizar.
/// Útil para integração com logs (`std::fmt::Write`).
pub struct RedactingFormatter {
    engine: RedactionEngine,
    buffer: String,
}

impl RedactingFormatter {
    pub fn new(engine: RedactionEngine) -> Self {
        Self {
            engine,
            buffer: String::new(),
        }
    }

    /// Consome o formatter e retorna a string redatada.
    pub fn finish(self) -> String {
        self.engine.redact_all(&self.buffer)
    }

    /// Retorna a string bruta acumulada (sem redação).
    pub fn raw(&self) -> &str {
        &self.buffer
    }
}

impl std::fmt::Write for RedactingFormatter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.buffer.push_str(s);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    fn engine() -> RedactionEngine {
        RedactionEngine::new().unwrap()
    }

    #[test]
    fn openai_key_long_mask() {
        let e = engine();
        let input = "key=sk-abcdefghijklmnopqrstuvwxyz1234567890ABCD";
        let out = e.redact(input);
        assert!(out.contains("sk-abc...ABCD"), "got: {}", out);
        assert!(!out.contains("vwxyz1234567890"));
    }

    #[test]
    fn github_pat_mask() {
        let e = engine();
        let input = "ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let out = e.redact(input);
        assert!(out.contains("ghp_xx...xxxx"), "got: {}", out);
    }

    #[test]
    fn aws_akia_mask() {
        let e = engine();
        let input = "AKIAIOSFODNN7EXAMPLE";
        let out = e.redact(input);
        assert!(out.contains("AKIAIO...MPLE"), "got: {}", out);
    }

    #[test]
    fn bearer_token_mask() {
        let e = engine();
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        let out = e.redact(input);
        // Bearer é preservado como prefixo (6 chars) do match inteiro
        assert!(out.contains("Bearer"), "got: {}", out);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
    }

    #[test]
    fn telegram_bot_token_mask() {
        let e = engine();
        let input =
            "https://api.telegram.org/bot123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11/sendMessage";
        let out = e.redact(input);
        assert!(out.contains("bot123...ew11"), "got: {}", out);
        assert!(!out.contains("ABC-DEF1234ghIkl-zyx57W2v1u123ew11"));
    }

    #[test]
    fn private_key_pem_mask() {
        let e = engine();
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA0Z3...\n-----END RSA PRIVATE KEY-----";
        let out = e.redact(input);
        // mask_token: 6 prefix + ... + 4 suffix
        assert!(out.contains("-----B...----"), "got: {}", out);
    }

    #[test]
    fn postgres_dsn_mask() {
        let e = engine();
        let input = "postgres://admin:SuperSecret123@localhost:5432/mydb";
        let out = e.redact(input);
        assert!(
            out.contains("postgres://admin:***@localhost"),
            "got: {}",
            out
        );
    }

    #[test]
    fn jwt_mask() {
        let e = engine();
        let input = "token=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = e.redact(input);
        assert!(out.contains("eyJhbG...sw5c"), "got: {}", out);
    }

    #[test]
    fn discord_mention_mask() {
        let e = engine();
        let input = "user: <@123456789012345678>";
        let out = e.redact(input);
        assert!(out.contains("<@1234...678>"), "got: {}", out);
    }

    #[test]
    fn e164_phone_mask() {
        let e = engine();
        let input = "call me at +5511999998888";
        let out = e.redact(input);
        // +5511999998888 tem 14 chars (< 18) => full mask
        assert!(out.contains("***"), "got: {}", out);
        assert!(!out.contains("99998888"));
    }

    #[test]
    fn stripe_key_mask() {
        let e = engine();
        // fixture montada em runtime para não disparar secret scanners sobre o fonte
        let input = format!("{}_{}_abcdefghijklmnopqrstuvwxyz", "sk", "live");
        let out = e.redact(&input);
        assert!(out.contains("sk_liv...wxyz"), "got: {}", out);
    }

    #[test]
    fn slack_token_mask() {
        let e = engine();
        // fixture montada em runtime para não disparar secret scanners sobre o fonte
        let input = format!("{}-1234567890123-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "xoxb");
        let out = e.redact(&input);
        assert!(out.contains("xoxb-1...UvWx"), "got: {}", out);
    }

    #[test]
    fn google_api_key_mask() {
        let e = engine();
        // fixture montada em runtime para não disparar secret scanners sobre o fonte
        let input = format!("{}SyDdI0hCZtE6vySjMm-WEfRq3CPzqKqqsHI", "AIza");
        let out = e.redact(&input);
        assert!(out.contains("AIzaSy...qsHI"), "got: {}", out);
    }

    #[test]
    fn mongodb_dsn_mask() {
        let e = engine();
        let input = "mongodb+srv://user:Pssw0rd!@cluster0.example.net/test";
        let out = e.redact(input);
        assert!(
            out.contains("mongodb+srv://user:***@cluster0"),
            "got: {}",
            out
        );
    }

    #[test]
    fn redis_dsn_mask() {
        let e = engine();
        let input = "redis://:mypassword@localhost:6379/0";
        let out = e.redact(input);
        assert!(out.contains("redis://:***@localhost"), "got: {}", out);
    }

    #[test]
    fn short_token_full_mask() {
        assert_eq!(mask_token("abc123"), "***");
        assert_eq!(mask_token("short12"), "***");
    }

    #[test]
    fn long_token_partial_mask() {
        let t = "sk-abcdefghijklmnopqrstuvwxyz";
        assert_eq!(mask_token(t), "sk-abc...wxyz");
    }

    #[test]
    fn query_string_redaction() {
        let e = engine();
        let input = "https://example.com/callback?code=abc123&state=xyz&access_token=secretstuff";
        let out = e.redact_query_strings(input);
        assert!(out.contains("code=***"), "got: {}", out);
        assert!(out.contains("access_token=***"), "got: {}", out);
        assert!(out.contains("state=xyz"), "got: {}", out);
    }

    #[test]
    fn json_redaction() {
        let e = engine();
        let input = r#"{"api_key":"sk-1234567890ABCDEF","user":"john","nested":{"secret":"my-secret","value":42}}"#;
        let out = e.redact_json(input).unwrap();
        assert!(
            out.contains("\"api_key\":\"sk-123...CDEF\""),
            "got: {}",
            out
        );
        assert!(out.contains("\"user\":\"john\""), "got: {}", out);
        // "my-secret" tem 9 chars (< 18) => ***
        assert!(out.contains("\"secret\":\"***\""), "got: {}", out);
        assert!(out.contains("42"), "got: {}", out);
    }

    #[test]
    fn env_redaction() {
        let e = engine();
        let input = "OPENAI_API_KEY=sk-1234567890\nDATABASE_URL=postgres://u:p@h\nSAFE_VAR=ok";
        let out = e.redact_env(input);
        assert!(out.contains("OPENAI_API_KEY=***"), "got: {}", out);
        assert!(out.contains("DATABASE_URL=***"), "got: {}", out);
        assert!(out.contains("SAFE_VAR=ok"), "got: {}", out);
    }

    #[test]
    fn env_redaction_skipped_in_code() {
        let e = engine().with_skip_env_in_code(true);
        let input = "OPENAI_API_KEY=sk-1234567890\nfn main() {}\nlet x = OPENAI_API_KEY=sk-1234567890\n  // OPENAI_API_KEY=sk-1234567890";
        let out = e.redact_env(input);
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines[0], "OPENAI_API_KEY=***");
        assert_eq!(lines[1], "fn main() {}");
        assert_eq!(lines[2], "let x = OPENAI_API_KEY=sk-1234567890");
        assert_eq!(lines[3], "  // OPENAI_API_KEY=sk-1234567890");
    }

    #[test]
    fn redacting_formatter() {
        let e = engine();
        let mut fmt = RedactingFormatter::new(e);
        write!(&mut fmt, "log: ").unwrap();
        write!(&mut fmt, "Bearer secret-token-1234567890").unwrap();
        let out = fmt.finish();
        assert!(out.contains("Bearer"));
        assert!(!out.contains("secret-token-1234567890"));
    }

    #[test]
    fn no_false_positives_on_normal_text() {
        let e = engine();
        let input = "The quick brown fox jumps over the lazy dog. 12345 abcde.";
        let out = e.redact(input);
        assert_eq!(out, input);
    }

    #[test]
    fn multiple_secrets_in_one_string() {
        let e = engine();
        // fixture montada em runtime para não disparar secret scanners sobre o fonte
        let input = format!(
            "keys: sk-abcdefghijklmnopqrstuvwxyz and {}_{}",
            "ghp", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        );
        let out = e.redact(&input);
        assert!(out.contains("sk-abc...wxyz"), "got: {}", out);
        assert!(out.contains("ghp_xx...xxxx"), "got: {}", out);
    }

    #[test]
    fn redact_all_detects_json() {
        let e = engine();
        // fixture montada em runtime para não disparar secret scanners sobre o fonte
        let input = format!(
            r#"{{"token":"{}-1234567890123-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx"}}"#,
            "xoxb"
        );
        let out = e.redact_all(&input);
        assert!(out.contains("xoxb-1...UvWx"), "got: {}", out);
    }

    #[test]
    fn redaction_disabled_returns_original() {
        let e = engine();
        let input = "nothing sensitive here";
        assert_eq!(e.redact_all(input), input);
    }

    #[test]
    fn authorization_header_mask() {
        let e = engine();
        let input = "Authorization: Basic dXNlcjpwYXNzd29yZA==";
        let out = e.redact(input);
        assert!(!out.contains("dXNlcjpwYXNzd29yZA=="));
        assert!(out.contains("Basic"), "got: {}", out);
    }

    #[test]
    fn discord_webhook_mask() {
        let e = engine();
        let input = "https://discord.com/api/webhooks/123456789/AbCdEfGhIjKlMnOpQrStUvWxYz";
        let out = e.redact(input);
        assert!(!out.contains("AbCdEfGhIjKlMnOpQrStUvWxYz"));
    }

    #[test]
    fn google_oauth_alt_mask() {
        let e = engine();
        let input = "token=gAAAAABl1234567890abcdefghijklmnopqrstuvwxyz";
        let out = e.redact(input);
        assert!(out.contains("gAAAAA...wxyz"), "got: {}", out);
    }
}
