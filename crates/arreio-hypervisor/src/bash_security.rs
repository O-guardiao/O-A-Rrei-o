//! Bash Security — 23 checks semânticos numerados (GAP-007).
//!
//! Substitui a abordagem simplista de regex blocklist por validações
//! semânticas específicas, cada uma com ID numerado para telemetria
//! e rastreabilidade.
//!
//! Cada check retorna `Result<(), BashSecurityViolation>` para o primeiro
//! padrão detectado, permitindo short-circuit sem varrer todos os checks.

use regex::Regex;
use thiserror::Error;

/// Identificador numérico de cada check de segurança bash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BashCheckId {
    IncompleteCommands = 1,
    JqSystemFunction = 2,
    JqFileArguments = 3,
    ObfuscatedFlags = 4,
    ShellMetacharacters = 5,
    DangerousVariables = 6,
    Newlines = 7,
    CommandSubstitution = 8,
    InputRedirection = 9,
    OutputRedirection = 10,
    IfsInjection = 11,
    GitCommitSubstitution = 12,
    ProcEnvironAccess = 13,
    MalformedTokenInjection = 14,
    BackslashEscapedWhitespace = 15,
    BraceExpansion = 16,
    ControlCharacters = 17,
    UnicodeWhitespace = 18,
    MidWordHash = 19,
    ZshDangerousCommands = 20,
    BackslashEscapedOperators = 21,
    CommentQuoteDesync = 22,
    QuotedNewline = 23,
}

impl BashCheckId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IncompleteCommands => "INCOMPLETE_COMMANDS",
            Self::JqSystemFunction => "JQ_SYSTEM_FUNCTION",
            Self::JqFileArguments => "JQ_FILE_ARGUMENTS",
            Self::ObfuscatedFlags => "OBFUSCATED_FLAGS",
            Self::ShellMetacharacters => "SHELL_METACHARACTERS",
            Self::DangerousVariables => "DANGEROUS_VARIABLES",
            Self::Newlines => "NEWLINES",
            Self::CommandSubstitution => "COMMAND_SUBSTITUTION",
            Self::InputRedirection => "INPUT_REDIRECTION",
            Self::OutputRedirection => "OUTPUT_REDIRECTION",
            Self::IfsInjection => "IFS_INJECTION",
            Self::GitCommitSubstitution => "GIT_COMMIT_SUBSTITUTION",
            Self::ProcEnvironAccess => "PROC_ENVIRON_ACCESS",
            Self::MalformedTokenInjection => "MALFORMED_TOKEN_INJECTION",
            Self::BackslashEscapedWhitespace => "BACKSLASH_ESCAPED_WHITESPACE",
            Self::BraceExpansion => "BRACE_EXPANSION",
            Self::ControlCharacters => "CONTROL_CHARACTERS",
            Self::UnicodeWhitespace => "UNICODE_WHITESPACE",
            Self::MidWordHash => "MID_WORD_HASH",
            Self::ZshDangerousCommands => "ZSH_DANGEROUS_COMMANDS",
            Self::BackslashEscapedOperators => "BACKSLASH_ESCAPED_OPERATORS",
            Self::CommentQuoteDesync => "COMMENT_QUOTE_DESYNC",
            Self::QuotedNewline => "QUOTED_NEWLINE",
        }
    }

    pub fn telemetry_key(&self) -> String {
        format!("security::bash_check::{}", self.as_str())
    }
}

/// Violação de segurança detectada por um dos 23 checks.
#[derive(Debug, Error)]
#[error("bash security check #{id}: {description}", id = self.check_id.as_str())]
pub struct BashSecurityViolation {
    pub check_id: BashCheckId,
    pub description: String,
}

/// Motor de 23 checks semânticos para comandos bash.
pub struct BashSecurityChecker {
    re_incomplete: Regex,
    re_jq_system: Regex,
    re_jq_file: Regex,
    re_obfuscated_hex: Regex,
    re_obfuscated_octal: Regex,
    re_dangerous_vars: Regex,
    re_cmd_subst_dollar: Regex,
    re_cmd_subst_backtick: Regex,
    re_input_redir: Regex,
    re_output_redir: Regex,
    re_ifs_injection: Regex,
    re_git_commit_subst: Regex,
    re_proc_environ: Regex,
    re_brace_expansion: Regex,
    re_unicode_ws: Regex,
    re_mid_word_hash: Regex,
    re_zsh_dangerous: Regex,
    re_backslash_operator: Regex,
    re_quoted_newline: Regex,
}

impl BashSecurityChecker {
    pub fn new() -> Self {
        Self {
            // 1. Comandos incompletos (trailing pipe, &&, ||, ;, &, |)
            re_incomplete: Regex::new(r"[|&;]\s*$").unwrap(),

            // 2. jq system(), input, debug (execução arbitrária via jq)
            re_jq_system: Regex::new(r"(?i)\bjq\b.*\b(system|input|debug|env)\s*\(").unwrap(),

            // 3. jq com argumentos de arquivo sensíveis
            re_jq_file: Regex::new(r"(?i)\bjq\b.*\s(--rawfile|--slurpfile|--jsonargs)\b").unwrap(),

            // 4. Flags ofuscadas via \x ou \0 encoding
            re_obfuscated_hex: Regex::new(r"\\x[0-9a-fA-F]{2}").unwrap(),
            re_obfuscated_octal: Regex::new(r"\\[0-7]{3}").unwrap(),

            // 5-6: variáveis perigosas (PATH, LD_PRELOAD, etc.)
            re_dangerous_vars: Regex::new(
                r"(?i)\b(PATH|LD_PRELOAD|LD_LIBRARY_PATH|DYLD_INSERT_LIBRARIES|PYTHONPATH|NODE_PATH|RUBYLIB|PERL5LIB|CLASSPATH|PROMPT_COMMAND|ENV|BASH_ENV|CDPATH|GLOBIGNORE|SHELLOPTS|BASHOPTS|PS1|PS4|IFS)\s*="
            ).unwrap(),

            // 8. Command substitution ($() e backticks)
            re_cmd_subst_dollar: Regex::new(r"\$\(").unwrap(),
            re_cmd_subst_backtick: Regex::new(r"`[^`]+`").unwrap(),

            // 9. Input redirection de paths sensíveis
            re_input_redir: Regex::new(
                r"<\s*(/etc/shadow|/etc/passwd|/proc/\d+/|/dev/|~/.ssh/|.*\.pem\b|.*\.key\b|.*id_rsa)"
            ).unwrap(),

            // 10. Output redirection a paths sensíveis
            re_output_redir: Regex::new(
                r">{1,2}\s*(/etc/|/usr/|/bin/|/sbin/|/dev/|/boot/|/proc/|/sys/|C:\\Windows|C:\\Program)"
            ).unwrap(),

            // 11. IFS injection
            re_ifs_injection: Regex::new(r"(?i)\bIFS\s*=").unwrap(),

            // 12. Git commit com command substitution
            re_git_commit_subst: Regex::new(r"(?i)git\s+commit\s+.*-m\s+.*(\$\(|`)").unwrap(),

            // 13. Acesso a /proc/*/environ
            re_proc_environ: Regex::new(r"/proc/\d+/environ|/proc/self/environ").unwrap(),

            // 16. Brace expansion perigosa (ex: {rm,-rf,/})
            re_brace_expansion: Regex::new(
                r"\{[^}]*(rm|chmod|dd|mkfs|kill|shutdown|reboot|halt|poweroff)[^}]*\}"
            ).unwrap(),

            // 18. Unicode whitespace (homoglyphs)
            re_unicode_ws: Regex::new(
                r"[\x{00A0}\x{1680}\x{2000}-\x{200B}\x{202F}\x{205F}\x{FEFF}]"
            ).unwrap(),

            // 19. Mid-word hash (pode truncar parsing)
            re_mid_word_hash: Regex::new(r"\S#\S").unwrap(),

            // 20. zsh dangerous commands
            re_zsh_dangerous: Regex::new(
                r"(?i)\b(zsh\s+-c|autoload\s+-Uz|zmodload|zle\s+-N)\b"
            ).unwrap(),

            // 21. Backslash antes de operadores (bypass de parsing)
            re_backslash_operator: Regex::new(r"\\[|;&]").unwrap(),

            // 23. Quoted newlines (injeção via strings multilinha)
            re_quoted_newline: Regex::new(r#""[^"]*\n[^"]*""#).unwrap(),
        }
    }

    /// Executa todos os 23 checks em sequência.
    /// Retorna Ok(()) se nenhum check falhar, ou Err com a primeira violação.
    pub fn check_all(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        self.check_incomplete_commands(cmd)?;
        self.check_jq_system_function(cmd)?;
        self.check_jq_file_arguments(cmd)?;
        self.check_obfuscated_flags(cmd)?;
        self.check_shell_metacharacters(cmd)?;
        self.check_dangerous_variables(cmd)?;
        self.check_newlines(cmd)?;
        self.check_command_substitution(cmd)?;
        self.check_input_redirection(cmd)?;
        self.check_output_redirection(cmd)?;
        self.check_ifs_injection(cmd)?;
        self.check_git_commit_substitution(cmd)?;
        self.check_proc_environ_access(cmd)?;
        self.check_malformed_token_injection(cmd)?;
        self.check_backslash_escaped_whitespace(cmd)?;
        self.check_brace_expansion(cmd)?;
        self.check_control_characters(cmd)?;
        self.check_unicode_whitespace(cmd)?;
        self.check_mid_word_hash(cmd)?;
        self.check_zsh_dangerous_commands(cmd)?;
        self.check_backslash_escaped_operators(cmd)?;
        self.check_comment_quote_desync(cmd)?;
        self.check_quoted_newline(cmd)?;
        Ok(())
    }

    /// Executa todos os checks e retorna TODAS as violações encontradas.
    pub fn check_all_collect(&self, cmd: &str) -> Vec<BashSecurityViolation> {
        let checks: Vec<fn(&Self, &str) -> Result<(), BashSecurityViolation>> = vec![
            Self::check_incomplete_commands,
            Self::check_jq_system_function,
            Self::check_jq_file_arguments,
            Self::check_obfuscated_flags,
            Self::check_shell_metacharacters,
            Self::check_dangerous_variables,
            Self::check_newlines,
            Self::check_command_substitution,
            Self::check_input_redirection,
            Self::check_output_redirection,
            Self::check_ifs_injection,
            Self::check_git_commit_substitution,
            Self::check_proc_environ_access,
            Self::check_malformed_token_injection,
            Self::check_backslash_escaped_whitespace,
            Self::check_brace_expansion,
            Self::check_control_characters,
            Self::check_unicode_whitespace,
            Self::check_mid_word_hash,
            Self::check_zsh_dangerous_commands,
            Self::check_backslash_escaped_operators,
            Self::check_comment_quote_desync,
            Self::check_quoted_newline,
        ];
        checks.iter().filter_map(|f| f(self, cmd).err()).collect()
    }

    // ── Check 1: Comandos incompletos ───────────────────────────────────────

    fn check_incomplete_commands(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_incomplete.is_match(cmd.trim()) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::IncompleteCommands,
                description: "comando incompleto: termina com operador pendente".into(),
            });
        }
        Ok(())
    }

    // ── Check 2: jq system() / input / debug ────────────────────────────────

    fn check_jq_system_function(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_jq_system.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::JqSystemFunction,
                description: "jq com função perigosa (system/input/debug/env)".into(),
            });
        }
        Ok(())
    }

    // ── Check 3: jq file arguments ──────────────────────────────────────────

    fn check_jq_file_arguments(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_jq_file.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::JqFileArguments,
                description: "jq com acesso a arquivo externo (--rawfile/--slurpfile)".into(),
            });
        }
        Ok(())
    }

    // ── Check 4: Flags ofuscadas ────────────────────────────────────────────

    fn check_obfuscated_flags(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_obfuscated_hex.is_match(cmd) || self.re_obfuscated_octal.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::ObfuscatedFlags,
                description: "flags ofuscadas via encoding hex/octal detectadas".into(),
            });
        }
        Ok(())
    }

    // ── Check 5: Shell metacharacters perigosos ─────────────────────────────

    fn check_shell_metacharacters(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        let dangerous_patterns = [
            ("eval ", "eval pode executar código arbitrário"),
            ("source ", "source pode executar scripts externos"),
            (". /", "dot-source pode executar scripts externos"),
        ];
        for (pattern, reason) in &dangerous_patterns {
            if cmd.contains(pattern) {
                return Err(BashSecurityViolation {
                    check_id: BashCheckId::ShellMetacharacters,
                    description: format!("metacaractere perigoso: {}", reason),
                });
            }
        }
        Ok(())
    }

    // ── Check 6: Variáveis perigosas ────────────────────────────────────────

    fn check_dangerous_variables(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        // IFS é tratado separadamente no check 11
        if self.re_dangerous_vars.is_match(cmd) && !cmd.contains("IFS=") {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::DangerousVariables,
                description: "atribuição a variável de ambiente perigosa".into(),
            });
        }
        Ok(())
    }

    // ── Check 7: Newlines literais ──────────────────────────────────────────

    fn check_newlines(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        let unquoted = strip_quoted_regions(cmd);
        if unquoted.contains('\n') {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::Newlines,
                description: "newline literal fora de string pode injetar comandos".into(),
            });
        }
        Ok(())
    }

    // ── Check 8: Command substitution ───────────────────────────────────────

    fn check_command_substitution(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        let unquoted = strip_single_quoted(cmd);
        if self.re_cmd_subst_dollar.is_match(&unquoted)
            || self.re_cmd_subst_backtick.is_match(&unquoted)
        {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::CommandSubstitution,
                description: "command substitution ($() ou backticks) detectada".into(),
            });
        }
        Ok(())
    }

    // ── Check 9: Input redirection sensível ─────────────────────────────────

    fn check_input_redirection(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_input_redir.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::InputRedirection,
                description: "redirecionamento de entrada de path sensível".into(),
            });
        }
        Ok(())
    }

    // ── Check 10: Output redirection sensível ───────────────────────────────

    fn check_output_redirection(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_output_redir.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::OutputRedirection,
                description: "redirecionamento de saída para path sensível".into(),
            });
        }
        Ok(())
    }

    // ── Check 11: IFS injection ─────────────────────────────────────────────

    fn check_ifs_injection(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_ifs_injection.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::IfsInjection,
                description: "manipulação de IFS pode comprometer word splitting".into(),
            });
        }
        Ok(())
    }

    // ── Check 12: Git commit com substitution ───────────────────────────────

    fn check_git_commit_substitution(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_git_commit_subst.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::GitCommitSubstitution,
                description: "git commit com command substitution na mensagem".into(),
            });
        }
        Ok(())
    }

    // ── Check 13: /proc/*/environ ───────────────────────────────────────────

    fn check_proc_environ_access(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_proc_environ.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::ProcEnvironAccess,
                description: "acesso a /proc/*/environ pode expor segredos".into(),
            });
        }
        Ok(())
    }

    // ── Check 14: Malformed token injection ─────────────────────────────────

    fn check_malformed_token_injection(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        // Detecta tentativas de injetar tokens via strings malformadas
        // Ex: }"; rm -rf / #  — fechando uma interpolação e injetando
        let suspicious = [r#"}"; "#, r#"'); "#, r#"}'; "#, "$(IFS=", "${IFS}"];
        for pattern in &suspicious {
            if cmd.contains(pattern) {
                return Err(BashSecurityViolation {
                    check_id: BashCheckId::MalformedTokenInjection,
                    description: "possível injeção via token malformado".into(),
                });
            }
        }
        Ok(())
    }

    // ── Check 15: Backslash-escaped whitespace ──────────────────────────────

    fn check_backslash_escaped_whitespace(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        // Line continuation (\<newline>) pode esconder injeção
        if cmd.contains("\\\n") {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::BackslashEscapedWhitespace,
                description: "line continuation pode esconder injeção de comandos".into(),
            });
        }
        // Backslash-space em posições suspeitas (mid-argument)
        if cmd.contains("\\ ") && cmd.len() > 100 {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::BackslashEscapedWhitespace,
                description: "backslash-space em comando longo pode ser ofuscação".into(),
            });
        }
        Ok(())
    }

    // ── Check 16: Brace expansion perigosa ──────────────────────────────────

    fn check_brace_expansion(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_brace_expansion.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::BraceExpansion,
                description: "brace expansion contendo comando perigoso".into(),
            });
        }
        // Detecta brace expansion numérica perigosa: {1..1000000}
        let re_numeric = Regex::new(r"\{(\d+)\.\.(\d+)\}").unwrap();
        for cap in re_numeric.captures_iter(cmd) {
            if let (Some(a), Some(b)) = (cap.get(1), cap.get(2)) {
                if let (Ok(va), Ok(vb)) = (a.as_str().parse::<u64>(), b.as_str().parse::<u64>()) {
                    let range = if va > vb { va - vb } else { vb - va };
                    if range > 10000 {
                        return Err(BashSecurityViolation {
                            check_id: BashCheckId::BraceExpansion,
                            description: format!(
                                "brace expansion numérica excessiva: range {}",
                                range
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    // ── Check 17: Control characters ────────────────────────────────────────

    fn check_control_characters(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        for ch in cmd.chars() {
            if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                return Err(BashSecurityViolation {
                    check_id: BashCheckId::ControlCharacters,
                    description: format!("caractere de controle U+{:04X} detectado", ch as u32),
                });
            }
        }
        Ok(())
    }

    // ── Check 18: Unicode whitespace ────────────────────────────────────────

    fn check_unicode_whitespace(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_unicode_ws.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::UnicodeWhitespace,
                description: "whitespace Unicode não-ASCII pode confundir o parser".into(),
            });
        }
        Ok(())
    }

    // ── Check 19: Mid-word hash ─────────────────────────────────────────────

    fn check_mid_word_hash(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        // Ignora dentro de strings e URLs
        let unquoted = strip_quoted_regions(cmd);
        // Ignora padrões comuns como #include, C# etc.
        if self.re_mid_word_hash.is_match(&unquoted) && !unquoted.contains("http") {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::MidWordHash,
                description: "hash mid-word pode causar truncamento inesperado".into(),
            });
        }
        Ok(())
    }

    // ── Check 20: zsh dangerous commands ────────────────────────────────────

    fn check_zsh_dangerous_commands(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_zsh_dangerous.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::ZshDangerousCommands,
                description: "comando zsh perigoso detectado".into(),
            });
        }
        Ok(())
    }

    // ── Check 21: Backslash-escaped operators ───────────────────────────────

    fn check_backslash_escaped_operators(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_backslash_operator.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::BackslashEscapedOperators,
                description: "operador de shell escapado com backslash pode ser bypass".into(),
            });
        }
        Ok(())
    }

    // ── Check 22: Comment/quote desync ──────────────────────────────────────

    fn check_comment_quote_desync(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        let mut in_single = false;
        let mut in_double = false;
        let mut prev_was_backslash = false;

        for ch in cmd.chars() {
            if prev_was_backslash {
                prev_was_backslash = false;
                continue;
            }
            match ch {
                '\\' if !in_single => {
                    prev_was_backslash = true;
                }
                '\'' if !in_double => {
                    in_single = !in_single;
                }
                '"' if !in_single => {
                    in_double = !in_double;
                }
                '#' if !in_single && !in_double => {
                    // Comentário legítimo — ok
                    return Ok(());
                }
                _ => {}
            }
        }

        // Se saímos do loop com quotes não fechadas, é um desync
        if in_single || in_double {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::CommentQuoteDesync,
                description: "aspas não fechadas podem causar desync de parsing".into(),
            });
        }
        Ok(())
    }

    // ── Check 23: Quoted newline ────────────────────────────────────────────

    fn check_quoted_newline(&self, cmd: &str) -> Result<(), BashSecurityViolation> {
        if self.re_quoted_newline.is_match(cmd) {
            return Err(BashSecurityViolation {
                check_id: BashCheckId::QuotedNewline,
                description: "newline dentro de string quoted pode esconder comandos".into(),
            });
        }
        Ok(())
    }
}

impl Default for BashSecurityChecker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Utilitários ──────────────────────────────────────────────────────────────

/// Remove regiões entre aspas simples e duplas, retornando o restante.
fn strip_quoted_regions(cmd: &str) -> String {
    let mut result = String::with_capacity(cmd.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_backslash = false;

    for ch in cmd.chars() {
        if prev_backslash {
            prev_backslash = false;
            if !in_single && !in_double {
                result.push(ch);
            }
            continue;
        }
        match ch {
            '\\' if !in_single => {
                prev_backslash = true;
                if !in_double {
                    result.push(ch);
                }
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            _ => {
                if !in_single && !in_double {
                    result.push(ch);
                }
            }
        }
    }
    result
}

/// Remove conteúdo entre aspas simples (onde $ e ` não são interpretados).
fn strip_single_quoted(cmd: &str) -> String {
    let mut result = String::with_capacity(cmd.len());
    let mut in_single = false;

    for ch in cmd.chars() {
        if ch == '\'' {
            in_single = !in_single;
        } else if !in_single {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> BashSecurityChecker {
        BashSecurityChecker::new()
    }

    // ── Check 1: Incomplete Commands ────────────────────────────────────────

    #[test]
    fn check01_incomplete_trailing_pipe() {
        let c = checker();
        assert_eq!(
            c.check_incomplete_commands("echo foo |")
                .unwrap_err()
                .check_id,
            BashCheckId::IncompleteCommands
        );
    }

    #[test]
    fn check01_complete_command_ok() {
        let c = checker();
        assert!(c.check_incomplete_commands("echo foo | grep bar").is_ok());
    }

    // ── Check 2: jq system() ────────────────────────────────────────────────

    #[test]
    fn check02_jq_system() {
        let c = checker();
        assert_eq!(
            c.check_jq_system_function(r#"jq '.x | system("whoami")'"#)
                .unwrap_err()
                .check_id,
            BashCheckId::JqSystemFunction
        );
    }

    #[test]
    fn check02_jq_safe() {
        let c = checker();
        assert!(c.check_jq_system_function("jq '.name'").is_ok());
    }

    // ── Check 3: jq file arguments ──────────────────────────────────────────

    #[test]
    fn check03_jq_rawfile() {
        let c = checker();
        assert_eq!(
            c.check_jq_file_arguments("jq --rawfile data /etc/passwd .")
                .unwrap_err()
                .check_id,
            BashCheckId::JqFileArguments
        );
    }

    // ── Check 4: Obfuscated flags ───────────────────────────────────────────

    #[test]
    fn check04_hex_escape() {
        let c = checker();
        assert_eq!(
            c.check_obfuscated_flags(r"echo $'\x72\x6d'")
                .unwrap_err()
                .check_id,
            BashCheckId::ObfuscatedFlags
        );
    }

    #[test]
    fn check04_normal_flags_ok() {
        let c = checker();
        assert!(c.check_obfuscated_flags("ls -la").is_ok());
    }

    // ── Check 5: Shell metacharacters ───────────────────────────────────────

    #[test]
    fn check05_eval() {
        let c = checker();
        assert_eq!(
            c.check_shell_metacharacters("eval $cmd")
                .unwrap_err()
                .check_id,
            BashCheckId::ShellMetacharacters
        );
    }

    // ── Check 6: Dangerous variables ────────────────────────────────────────

    #[test]
    fn check06_ld_preload() {
        let c = checker();
        assert_eq!(
            c.check_dangerous_variables("LD_PRELOAD=/evil.so cmd")
                .unwrap_err()
                .check_id,
            BashCheckId::DangerousVariables
        );
    }

    #[test]
    fn check06_path_set() {
        let c = checker();
        assert_eq!(
            c.check_dangerous_variables("PATH=/tmp:$PATH cmd")
                .unwrap_err()
                .check_id,
            BashCheckId::DangerousVariables
        );
    }

    // ── Check 7: Newlines ───────────────────────────────────────────────────

    #[test]
    fn check07_newline_injection() {
        let c = checker();
        assert_eq!(
            c.check_newlines("echo safe\nrm -rf /")
                .unwrap_err()
                .check_id,
            BashCheckId::Newlines
        );
    }

    #[test]
    fn check07_no_newline_ok() {
        let c = checker();
        assert!(c.check_newlines("echo hello world").is_ok());
    }

    // ── Check 8: Command substitution ───────────────────────────────────────

    #[test]
    fn check08_dollar_paren() {
        let c = checker();
        assert_eq!(
            c.check_command_substitution("echo $(whoami)")
                .unwrap_err()
                .check_id,
            BashCheckId::CommandSubstitution
        );
    }

    #[test]
    fn check08_backtick() {
        let c = checker();
        assert_eq!(
            c.check_command_substitution("echo `id`")
                .unwrap_err()
                .check_id,
            BashCheckId::CommandSubstitution
        );
    }

    #[test]
    fn check08_single_quoted_ok() {
        let c = checker();
        assert!(c.check_command_substitution("echo '$(safe)'").is_ok());
    }

    // ── Check 9: Input redirection ──────────────────────────────────────────

    #[test]
    fn check09_sensitive_input() {
        let c = checker();
        assert_eq!(
            c.check_input_redirection("cat < /etc/shadow")
                .unwrap_err()
                .check_id,
            BashCheckId::InputRedirection
        );
    }

    // ── Check 10: Output redirection ────────────────────────────────────────

    #[test]
    fn check10_sensitive_output() {
        let c = checker();
        assert_eq!(
            c.check_output_redirection("echo x > /etc/crontab")
                .unwrap_err()
                .check_id,
            BashCheckId::OutputRedirection
        );
    }

    #[test]
    fn check10_safe_output_ok() {
        let c = checker();
        assert!(c.check_output_redirection("echo x > /tmp/out.txt").is_ok());
    }

    // ── Check 11: IFS injection ─────────────────────────────────────────────

    #[test]
    fn check11_ifs() {
        let c = checker();
        assert_eq!(
            c.check_ifs_injection("IFS=/ read a b")
                .unwrap_err()
                .check_id,
            BashCheckId::IfsInjection
        );
    }

    // ── Check 12: Git commit substitution ───────────────────────────────────

    #[test]
    fn check12_git_commit_subst() {
        let c = checker();
        assert_eq!(
            c.check_git_commit_substitution("git commit -m \"$(cat /etc/passwd)\"")
                .unwrap_err()
                .check_id,
            BashCheckId::GitCommitSubstitution
        );
    }

    // ── Check 13: /proc/environ ─────────────────────────────────────────────

    #[test]
    fn check13_proc_environ() {
        let c = checker();
        assert_eq!(
            c.check_proc_environ_access("cat /proc/self/environ")
                .unwrap_err()
                .check_id,
            BashCheckId::ProcEnvironAccess
        );
    }

    // ── Check 14: Malformed token injection ─────────────────────────────────

    #[test]
    fn check14_token_injection() {
        let c = checker();
        assert_eq!(
            c.check_malformed_token_injection(r#"echo "}"; rm -rf / #""#)
                .unwrap_err()
                .check_id,
            BashCheckId::MalformedTokenInjection
        );
    }

    // ── Check 15: Backslash-escaped whitespace ──────────────────────────────

    #[test]
    fn check15_line_continuation() {
        let c = checker();
        assert_eq!(
            c.check_backslash_escaped_whitespace("rm \\\n-rf /")
                .unwrap_err()
                .check_id,
            BashCheckId::BackslashEscapedWhitespace
        );
    }

    // ── Check 16: Brace expansion ───────────────────────────────────────────

    #[test]
    fn check16_brace_with_rm() {
        let c = checker();
        assert_eq!(
            c.check_brace_expansion("{rm,-rf,/}").unwrap_err().check_id,
            BashCheckId::BraceExpansion
        );
    }

    #[test]
    fn check16_numeric_range_excessive() {
        let c = checker();
        assert_eq!(
            c.check_brace_expansion("echo {1..999999}")
                .unwrap_err()
                .check_id,
            BashCheckId::BraceExpansion
        );
    }

    // ── Check 17: Control characters ────────────────────────────────────────

    #[test]
    fn check17_control_char() {
        let c = checker();
        assert_eq!(
            c.check_control_characters("echo \x01evil")
                .unwrap_err()
                .check_id,
            BashCheckId::ControlCharacters
        );
    }

    #[test]
    fn check17_tab_ok() {
        let c = checker();
        assert!(c.check_control_characters("echo\thello").is_ok());
    }

    // ── Check 18: Unicode whitespace ────────────────────────────────────────

    #[test]
    fn check18_nbsp() {
        let c = checker();
        assert_eq!(
            c.check_unicode_whitespace("rm\u{00A0}-rf /")
                .unwrap_err()
                .check_id,
            BashCheckId::UnicodeWhitespace
        );
    }

    // ── Check 19: Mid-word hash ─────────────────────────────────────────────

    #[test]
    fn check19_mid_hash() {
        let c = checker();
        assert_eq!(
            c.check_mid_word_hash("a#b").unwrap_err().check_id,
            BashCheckId::MidWordHash
        );
    }

    #[test]
    fn check19_line_comment_ok() {
        let c = checker();
        assert!(c.check_mid_word_hash("echo hello # comentário").is_ok());
    }

    // ── Check 20: zsh dangerous ─────────────────────────────────────────────

    #[test]
    fn check20_zsh_c() {
        let c = checker();
        assert_eq!(
            c.check_zsh_dangerous_commands("zsh -c 'rm -rf /'")
                .unwrap_err()
                .check_id,
            BashCheckId::ZshDangerousCommands
        );
    }

    // ── Check 21: Backslash-escaped operators ───────────────────────────────

    #[test]
    fn check21_escaped_pipe() {
        let c = checker();
        assert_eq!(
            c.check_backslash_escaped_operators("echo foo \\| bar")
                .unwrap_err()
                .check_id,
            BashCheckId::BackslashEscapedOperators
        );
    }

    // ── Check 22: Comment/quote desync ──────────────────────────────────────

    #[test]
    fn check22_unclosed_quote() {
        let c = checker();
        assert_eq!(
            c.check_comment_quote_desync("echo \"not closed")
                .unwrap_err()
                .check_id,
            BashCheckId::CommentQuoteDesync
        );
    }

    #[test]
    fn check22_balanced_ok() {
        let c = checker();
        assert!(c
            .check_comment_quote_desync("echo \"hello\" 'world'")
            .is_ok());
    }

    // ── Check 23: Quoted newline ────────────────────────────────────────────

    #[test]
    fn check23_quoted_newline() {
        let c = checker();
        assert_eq!(
            c.check_quoted_newline("echo \"line1\nline2\"")
                .unwrap_err()
                .check_id,
            BashCheckId::QuotedNewline
        );
    }

    // ── Integração: check_all ───────────────────────────────────────────────

    #[test]
    fn check_all_safe_command_passes() {
        let c = checker();
        assert!(c.check_all("cargo test --workspace").is_ok());
    }

    #[test]
    fn check_all_dangerous_fails() {
        let c = checker();
        assert!(c.check_all("LD_PRELOAD=/evil.so cmd").is_err());
    }

    #[test]
    fn check_all_collect_multiple_violations() {
        let c = checker();
        // Combina IFS injection + control character
        let violations = c.check_all_collect("IFS=/ \x01evil");
        assert!(violations.len() >= 2);
    }

    // ── Falsos positivos ────────────────────────────────────────────────────

    #[test]
    fn false_positive_normal_git_commit() {
        let c = checker();
        assert!(c.check_all("git commit -m 'fix: resolve bug'").is_ok());
    }

    #[test]
    fn false_positive_cargo_commands() {
        let c = checker();
        assert!(c.check_all("cargo build --release").is_ok());
        assert!(c.check_all("cargo test -p arreio-kernel").is_ok());
        assert!(c.check_all("cargo check --workspace").is_ok());
    }

    #[test]
    fn false_positive_echo_with_redirect() {
        let c = checker();
        assert!(c.check_all("echo hello > /tmp/test.txt").is_ok());
    }

    #[test]
    fn false_positive_jq_simple() {
        let c = checker();
        assert!(c.check_all("jq '.name' data.json").is_ok());
    }

    #[test]
    fn telemetry_key_format() {
        assert_eq!(
            BashCheckId::IncompleteCommands.telemetry_key(),
            "security::bash_check::INCOMPLETE_COMMANDS"
        );
    }
}
