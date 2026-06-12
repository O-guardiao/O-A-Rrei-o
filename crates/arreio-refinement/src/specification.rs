/// Especificação formal no estilo refinamento de Back/Morgan.
/// Frame: variáveis que podem ser modificadas.
/// Pre: pré-condição (predicado sobre o estado inicial).
/// Post: pós-condição (predicado sobre o estado final).
#[derive(Debug, Clone, PartialEq)]
pub struct SpecificationStatement {
    pub frame: Vec<String>,
    pub pre: String,
    pub post: String,
}

impl SpecificationStatement {
    pub fn new(frame: Vec<String>, pre: impl Into<String>, post: impl Into<String>) -> Self {
        Self {
            frame,
            pre: pre.into(),
            post: post.into(),
        }
    }

    /// Notação w : [pre, post]
    pub fn notation(&self) -> String {
        format!("w : [{}, {}]", self.pre, self.post)
    }

    /// Verificação heurística simples: verifica se o código parece satisfazer
    /// a pós-condição (busca variáveis do frame e padrões simples).
    pub fn is_satisfied_by(&self, code: &str) -> bool {
        let code = code.trim();
        if code.is_empty() {
            return false;
        }
        if self.post.trim() == "true" || self.post.trim().is_empty() {
            return true;
        }

        let code_lower = code.to_lowercase();

        // Se a pós-condição é uma igualdade "x == expr", verifica se há atribuição a x.
        if let Some((var, _expr)) = parse_equality(&self.post) {
            if self.frame.contains(&var) {
                let assign_pattern = format!("{} =", var);
                let let_pattern = format!("let {} ", var);
                let let_mut_pattern = format!("let mut {} ", var);
                if code_lower.contains(&assign_pattern.to_lowercase())
                    || code_lower.contains(&let_pattern.to_lowercase())
                    || code_lower.contains(&let_mut_pattern.to_lowercase())
                {
                    return true;
                }
            }
        }

        // Fallback: verifica se pelo menos uma variável do frame aparece no código
        self.frame
            .iter()
            .any(|var| code_lower.contains(&var.to_lowercase()))
    }
}

fn parse_equality(s: &str) -> Option<(String, String)> {
    let mut parts = s.splitn(2, "==");
    let var = parts.next()?.trim().to_string();
    let expr = parts.next()?.trim().to_string();
    if expr.is_empty() {
        None
    } else {
        Some((var, expr))
    }
}
