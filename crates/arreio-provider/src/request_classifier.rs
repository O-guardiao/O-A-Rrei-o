use crate::ChatRequest;
use regex::Regex;

/// Complexidade estimada de uma tarefa baseada em heurísticas do prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskComplexity {
    /// < 500 tokens estimados, sem code blocks ou math.
    Simple,
    /// 500–4000 tokens, ou com code blocks simples.
    Moderate,
    /// > 4000 tokens, ou com múltiplos code blocks, math complexo.
    Complex,
}

/// Nível de sensibilidade de dados detectado no prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensitivityLevel {
    /// Sem indicadores de sensibilidade.
    Low,
    /// Palavras-chave de dados pessoais ou business.
    Medium,
    /// PII explícito, credenciais, palavras de compliance.
    High,
}

/// Tipo de request inferido heurísticamente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestType {
    /// Perguntas rápidas, explicações curtas.
    QuickQuery,
    /// Geração ou refatoração de código.
    CodeGeneration,
    /// Tarefas com expressões matemáticas.
    MathTask,
    /// Processamento em lote, sumarização de múltiplos itens.
    BatchProcessing,
    /// Caso geral, não classificado.
    General,
}

/// Resultado da classificação determinística de um `ChatRequest`.
#[derive(Debug, Clone)]
pub struct ClassifiedRequest {
    pub complexity: TaskComplexity,
    pub sensitivity: SensitivityLevel,
    pub request_type: RequestType,
    pub estimated_tokens: u64,
    pub has_code_blocks: bool,
    pub has_math_expressions: bool,
}

/// Classificador determinístico de requests LLM.
///
/// **Nunca usa LLM** — todas as decisões são heurísticas léxicas compiladas.
/// Cada instância pré-compila as regex para evitar custo de re-compilação.
pub struct RequestClassifier {
    pii_re: Regex,
    credential_re: Regex,
    math_re: Regex,
    code_block_re: Regex,
}

impl Default for RequestClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestClassifier {
    /// Cria um novo classificador com regex pré-compiladas.
    pub fn new() -> Self {
        Self {
            pii_re: Regex::new(
                r"(?i)(ssn|social.security|cpf|passport|credit.card|cvv|dob|date.of.birth|phone.number|address.street)"
            ).expect("regex PII válido"),
            credential_re: Regex::new(
                r"(?i)(password|api[_-]?key|secret|token|private[_-]?key|credential|auth)"
            ).expect("regex credential válido"),
            math_re: Regex::new(
                r"(\$[^$]+\$|\\\([^)]+\\\)|\\\[[^\]]+\\\]|∫|∑|√|∂|∞|[0-9]+\s*[+\-*/^]\s*[0-9]+)"
            ).expect("regex math válido"),
            code_block_re: Regex::new(r"```|`[^`]+`").expect("regex code block válido"),
        }
    }

    /// Classifica um `ChatRequest` retornando estrutura completa de metadados.
    pub fn classify(&self, req: &ChatRequest) -> ClassifiedRequest {
        let text = format!("{} {}", req.system, req.user);
        let estimated_tokens = Self::estimate_tokens(&text);

        let has_code_blocks = self.detect_code_blocks(&text);
        let has_math_expressions = self.detect_math(&text);

        let complexity =
            Self::classify_complexity(estimated_tokens, has_code_blocks, has_math_expressions);
        let sensitivity = self.classify_sensitivity(&text);
        let request_type =
            Self::classify_request_type(&text, has_code_blocks, has_math_expressions, estimated_tokens);

        ClassifiedRequest {
            complexity,
            sensitivity,
            request_type,
            estimated_tokens,
            has_code_blocks,
            has_math_expressions,
        }
    }

    /// Estimativa rápida de tokens: ~4 caracteres por token (heurística inglesa/português).
    fn estimate_tokens(text: &str) -> u64 {
        (text.len() / 4) as u64
    }

    fn detect_code_blocks(&self, text: &str) -> bool {
        self.code_block_re.is_match(text)
    }

    fn detect_math(&self, text: &str) -> bool {
        self.math_re.is_match(text)
    }

    fn classify_complexity(
        tokens: u64,
        has_code: bool,
        has_math: bool,
    ) -> TaskComplexity {
        match tokens {
            _ if tokens > 4000 || (has_code && has_math) => TaskComplexity::Complex,
            _ if tokens > 500 || has_code || has_math => TaskComplexity::Moderate,
            _ => TaskComplexity::Simple,
        }
    }

    fn classify_sensitivity(&self, text: &str) -> SensitivityLevel {
        if self.pii_re.is_match(text) || self.credential_re.is_match(text) {
            return SensitivityLevel::High;
        }
        let lower = text.to_lowercase();
        if lower.contains("confidential")
            || lower.contains("proprietary")
            || lower.contains("nda")
            || lower.contains("internal")
            || lower.contains("revenue")
            || lower.contains("financial")
        {
            return SensitivityLevel::Medium;
        }
        SensitivityLevel::Low
    }

    fn classify_request_type(
        text: &str,
        has_code: bool,
        has_math: bool,
        estimated_tokens: u64,
    ) -> RequestType {
        let lower = text.to_lowercase();

        if has_math && !has_code {
            return RequestType::MathTask;
        }

        if has_code
            || lower.contains("code")
            || lower.contains("function")
            || lower.contains("implement")
            || lower.contains("refactor")
            || lower.contains("class")
            || lower.contains("struct")
        {
            return RequestType::CodeGeneration;
        }

        if lower.contains("batch")
            || lower.contains("process all")
            || lower.contains("summarize all")
            || lower.contains("for each")
        {
            return RequestType::BatchProcessing;
        }

        if lower.contains("quick")
            || lower.contains("what is")
            || lower.contains("explain")
            || lower.contains("who")
            || lower.contains("when")
            || lower.contains("where")
            || estimated_tokens < 500
        {
            return RequestType::QuickQuery;
        }

        RequestType::General
    }
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn req(user: &str) -> ChatRequest {
        ChatRequest::new("test", "system prompt", user)
    }

    #[test]
    fn classify_simple_query() {
        let c = RequestClassifier::new();
        let r = req("What is Rust?");
        let cls = c.classify(&r);
        assert_eq!(cls.complexity, TaskComplexity::Simple);
        assert_eq!(cls.request_type, RequestType::QuickQuery);
    }

    #[test]
    fn classify_code_generation() {
        let c = RequestClassifier::new();
        let r = req("Implement a function `fn add(a: i32, b: i32) -> i32`");
        let cls = c.classify(&r);
        assert_eq!(cls.request_type, RequestType::CodeGeneration);
        assert!(cls.has_code_blocks || cls.complexity != TaskComplexity::Simple);
    }

    #[test]
    fn classify_code_block() {
        let c = RequestClassifier::new();
        let r = req("Fix this code: ```rust\nfn main() {\n  println!(\"hello\");\n}\n```");
        let cls = c.classify(&r);
        assert!(cls.has_code_blocks);
        assert_eq!(cls.request_type, RequestType::CodeGeneration);
    }

    #[test]
    fn classify_math() {
        let c = RequestClassifier::new();
        let r = req("Calculate the integral of $x^2 + 3x$ from 0 to 1");
        let cls = c.classify(&r);
        assert!(cls.has_math_expressions);
        assert_eq!(cls.request_type, RequestType::MathTask);
    }

    #[test]
    fn classify_batch() {
        let c = RequestClassifier::new();
        let r = req("Process all files in the directory and summarize each one");
        let cls = c.classify(&r);
        assert_eq!(cls.request_type, RequestType::BatchProcessing);
    }

    #[test]
    fn classify_complex_long() {
        let c = RequestClassifier::new();
        // 16000 chars ~ 4000 tokens → threshold de Complex
        let long = "a".repeat(16000);
        let r = req(&long);
        let cls = c.classify(&r);
        assert_eq!(cls.complexity, TaskComplexity::Complex);
    }

    #[test]
    fn classify_moderate() {
        let c = RequestClassifier::new();
        let medium = "b".repeat(2000);
        let r = req(&medium);
        let cls = c.classify(&r);
        assert_eq!(cls.complexity, TaskComplexity::Moderate);
    }

    #[test]
    fn classify_sensitivity_high_pii() {
        let c = RequestClassifier::new();
        let r = req("My SSN is 123-45-6789 and my passport is AB123456");
        let cls = c.classify(&r);
        assert_eq!(cls.sensitivity, SensitivityLevel::High);
    }

    #[test]
    fn classify_sensitivity_high_credential() {
        let c = RequestClassifier::new();
        let r = req("The API key is sk-abc123 and password is secret123");
        let cls = c.classify(&r);
        assert_eq!(cls.sensitivity, SensitivityLevel::High);
    }

    #[test]
    fn classify_sensitivity_medium() {
        let c = RequestClassifier::new();
        let r = req("This is confidential financial data under NDA");
        let cls = c.classify(&r);
        assert_eq!(cls.sensitivity, SensitivityLevel::Medium);
    }

    #[test]
    fn classify_sensitivity_low() {
        let c = RequestClassifier::new();
        let r = req("What is the weather today?");
        let cls = c.classify(&r);
        assert_eq!(cls.sensitivity, SensitivityLevel::Low);
    }

    #[test]
    fn classify_general_fallback() {
        let c = RequestClassifier::new();
        // Texto longo o suficiente para não cair em QuickQuery (>500 tokens estimados)
        // e sem palavras-chave específicas de code/math/batch.
        let text = "Write a comprehensive analysis of modern computing paradigms and their implications for distributed systems architecture ".repeat(20);
        let r = req(&text);
        let cls = c.classify(&r);
        assert_eq!(cls.request_type, RequestType::General);
    }

    #[test]
    fn estimate_tokens_reasonable() {
        let text = "a".repeat(400);
        assert_eq!(RequestClassifier::estimate_tokens(&text), 100);
    }
}
