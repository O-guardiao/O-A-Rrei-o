use serde::{Deserialize, Serialize};

/// Declaração de especificação formal para refinamento.
///
/// Representa uma especificação no estilo precondição/pós-condição/frame,
/// usada para derivar contratos de Design by Contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpecificationStatement {
    /// Pré-condição: o que deve ser verdade antes da execução.
    pub pre: String,
    /// Pós-condição: o que deve ser verdade após a execução.
    pub post: String,
    /// Frame (invariante): o que deve ser preservado durante a execução.
    pub frame: String,
}

impl SpecificationStatement {
    /// Cria uma nova especificação.
    pub fn new(pre: impl Into<String>, post: impl Into<String>, frame: impl Into<String>) -> Self {
        Self {
            pre: pre.into(),
            post: post.into(),
            frame: frame.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_creation() {
        let spec = SpecificationStatement::new("x > 0", "x > 0", "x >= 0");
        assert_eq!(spec.pre, "x > 0");
        assert_eq!(spec.post, "x > 0");
        assert_eq!(spec.frame, "x >= 0");
    }
}
