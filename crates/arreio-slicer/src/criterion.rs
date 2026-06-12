//! Critério de slicing — define a linha e a variável alvo.

use anyhow::{ensure, Result};

/// Critério para execução de um program slice.
///
/// O slice será calculado a partir da variável em uma linha específica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceCriterion {
    pub line: usize,
    pub variable: String,
}

impl SliceCriterion {
    /// Constrói um novo critério.
    pub fn new(line: usize, variable: impl Into<String>) -> Self {
        Self {
            line,
            variable: variable.into(),
        }
    }

    /// Valida se o critério faz sentido para o código-fonte fornecido.
    ///
    /// A linha deve existir e a variável deve aparecer nela.
    pub fn validate(&self, source: &str) -> Result<()> {
        let lines: Vec<&str> = source.lines().collect();
        ensure!(
            self.line > 0 && self.line <= lines.len(),
            "Linha {} fora do intervalo (fonte tem {} linhas)",
            self.line,
            lines.len()
        );

        let target_line = lines[self.line - 1];
        ensure!(
            target_line.contains(&self.variable),
            "Variável '{}' não encontrada na linha {}",
            self.variable,
            self.line
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let c = SliceCriterion::new(5, "x");
        assert_eq!(c.line, 5);
        assert_eq!(c.variable, "x");
    }

    #[test]
    fn test_validate_ok() {
        let source = "let x = 1;\nlet y = x + 2;";
        let c = SliceCriterion::new(2, "x");
        assert!(c.validate(source).is_ok());
    }

    #[test]
    fn test_validate_line_out_of_range() {
        let source = "let x = 1;";
        let c = SliceCriterion::new(5, "x");
        assert!(c.validate(source).is_err());
    }

    #[test]
    fn test_validate_variable_not_found() {
        let source = "let x = 1;\nlet y = 2;";
        let c = SliceCriterion::new(2, "x");
        assert!(c.validate(source).is_err());
    }
}
