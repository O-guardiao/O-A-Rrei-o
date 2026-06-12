//! Forward slicing — instruções influenciadas pelo critério.

use crate::{slicer::SliceResult, SliceCriterion};
use anyhow::Result;
use std::collections::HashSet;

/// Computa o forward slice: linhas que usam a variável do critério
/// ou variáveis por ela influenciadas.
pub fn forward_slice(source: &str, criterion: &SliceCriterion) -> Result<SliceResult> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Ok(SliceResult {
            relevant_lines: vec![],
            source: source.to_string(),
        });
    }

    let mut relevant_vars: HashSet<String> = HashSet::from_iter([criterion.variable.clone()]);
    let mut relevant_lines: HashSet<usize> = HashSet::new();

    let start = (criterion.line - 1).min(lines.len() - 1);

    // Inclui a linha do critério se ela contiver a variável.
    if criterion.line > 0 && criterion.line <= lines.len() {
        let crit_line = lines[criterion.line - 1];
        if crit_line.contains(&criterion.variable) {
            relevant_lines.insert(criterion.line);
        }
    }

    // Percorre da linha do critério até o final.
    for idx in start..lines.len() {
        let line_text = lines[idx];
        let (assigned, used) = crate::slicer::parse_line(line_text);

        // Verifica se alguma variável relevante é usada nesta linha.
        let uses_relevant = used.iter().any(|u| relevant_vars.contains(u));

        // Se a linha atribui a uma variável relevante ou usa variável relevante,
        // ela faz parte do slice.
        let assigns_relevant = assigned
            .as_ref()
            .map_or(false, |a| relevant_vars.contains(a));

        if assigns_relevant || uses_relevant {
            relevant_lines.insert(idx + 1); // 1-based
                                            // Se há atribuição, a variável atribuída passa a ser relevante.
            if let Some(var) = assigned {
                relevant_vars.insert(var);
            }
        }
    }

    let mut relevant_lines: Vec<usize> = relevant_lines.into_iter().collect();
    relevant_lines.sort_unstable();

    Ok(SliceResult {
        relevant_lines,
        source: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SliceCriterion;

    #[test]
    fn test_forward_slice_simple() {
        let source = "let x = 1;\nlet y = x + 2;\nlet z = y * 3;";
        let criterion = SliceCriterion::new(1, "x");
        let result = forward_slice(source, &criterion).unwrap();
        // x influencia y (linha 2) e z (linha 3); linha 1 também é incluída (atribui x)
        assert_eq!(result.relevant_lines, vec![1, 2, 3]);
    }

    #[test]
    fn test_forward_slice_in_loop() {
        let source = "let mut i = 0;\nwhile i < 10 {\n    i = i + 1;\n}\nprintln!(\"{}\", i);";
        let criterion = SliceCriterion::new(1, "i");
        let result = forward_slice(source, &criterion).unwrap();
        // linha 1 (i=0), linha 2 (usa i), linha 3 (atribui i, usa i), linha 5 (usa i)
        assert_eq!(result.relevant_lines, vec![1, 2, 3, 5]);
    }

    #[test]
    fn test_forward_slice_empty_source() {
        let source = "";
        let criterion = SliceCriterion::new(1, "x");
        let result = forward_slice(source, &criterion).unwrap();
        assert!(result.relevant_lines.is_empty());
    }

    #[test]
    fn test_forward_slice_nonexistent_variable() {
        let source = "let x = 1;\nlet y = 2;";
        let criterion = SliceCriterion::new(1, "z");
        let result = forward_slice(source, &criterion).unwrap();
        // z não é usada em lugar nenhum.
        assert!(result.relevant_lines.is_empty());
    }

    #[test]
    fn test_forward_slice_preserves_order() {
        let source = "let a = 1;\nlet b = a;\nlet c = b;\nlet d = c;";
        let criterion = SliceCriterion::new(1, "a");
        let result = forward_slice(source, &criterion).unwrap();
        assert_eq!(result.relevant_lines, vec![1, 2, 3, 4]);
    }
}
