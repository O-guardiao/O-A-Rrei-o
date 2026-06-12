//! Integração de slicing com curadoria de contexto.
//!
//! Reduz um código-fonte ao conjunto mínimo de linhas relevantes para
//! múltiplos critérios de slicing, preservando a ordem original.

use crate::{ProgramSlicer, SliceCriterion, SliceDirection};
use anyhow::Result;
use std::collections::HashSet;

/// Curadoria de contexto via program slicing.
pub struct ContextCuration;

impl ContextCuration {
    /// Recebe um código-fonte e uma lista de critérios, retornando apenas
    /// as linhas relevantes para todos os critérios, ordenadas.
    pub fn curate(source: &str, criteria: Vec<SliceCriterion>) -> Result<String> {
        if source.is_empty() {
            return Ok(String::new());
        }

        let mut all_lines: HashSet<usize> = HashSet::new();

        for criterion in criteria {
            let result = ProgramSlicer::slice(source, &criterion, SliceDirection::Both)?;
            for line in result.relevant_lines {
                all_lines.insert(line);
            }
        }

        let mut sorted_lines: Vec<usize> = all_lines.into_iter().collect();
        sorted_lines.sort_unstable();

        let lines: Vec<&str> = source.lines().collect();
        let mut output = String::new();
        for line_no in sorted_lines {
            if line_no > 0 && line_no <= lines.len() {
                output.push_str(lines[line_no - 1]);
                output.push('\n');
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SliceCriterion;

    #[test]
    fn test_curate_multiple_criteria() {
        let source = "let a = 1;\nlet b = 2;\nlet c = a + b;\nlet d = c * 2;";
        let criteria = vec![SliceCriterion::new(3, "c"), SliceCriterion::new(4, "d")];
        let result = ContextCuration::curate(source, criteria).unwrap();
        let expected = "let a = 1;\nlet b = 2;\nlet c = a + b;\nlet d = c * 2;\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_curate_empty_source() {
        let source = "";
        let criteria = vec![SliceCriterion::new(1, "x")];
        let result = ContextCuration::curate(source, criteria).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_curate_preserves_order() {
        let source = "let z = 10;\nlet a = 1;\nlet b = a;\nlet c = b;";
        let criteria = vec![SliceCriterion::new(3, "b"), SliceCriterion::new(4, "c")];
        let result = ContextCuration::curate(source, criteria).unwrap();
        let expected = "let a = 1;\nlet b = a;\nlet c = b;\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_curate_single_criterion() {
        let source = "let x = 1;\nlet y = x + 1;\nlet z = y + 1;";
        let criteria = vec![SliceCriterion::new(2, "y")];
        let result = ContextCuration::curate(source, criteria).unwrap();
        // Both direction: backward (x, y) + forward (y, z)
        let expected = "let x = 1;\nlet y = x + 1;\nlet z = y + 1;\n";
        assert_eq!(result, expected);
    }
}
