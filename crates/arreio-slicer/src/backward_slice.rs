//! Backward slicing — instruções que influenciam o critério.

use crate::{slicer::SliceResult, SliceCriterion};
use anyhow::Result;
use std::collections::HashSet;

/// Computa o backward slice: linhas que atribuem à variável do critério
/// ou a variáveis que a influenciam indiretamente.
pub fn backward_slice(source: &str, criterion: &SliceCriterion) -> Result<SliceResult> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Ok(SliceResult {
            relevant_lines: vec![],
            source: source.to_string(),
        });
    }

    let mut relevant_vars: HashSet<String> = HashSet::from_iter([criterion.variable.clone()]);
    let mut relevant_lines: HashSet<usize> = HashSet::new();

    let max_line = criterion.line.min(lines.len());

    // Inclui a linha do critério se ela contiver a variável.
    if max_line > 0 {
        let crit_line = lines[max_line - 1];
        if crit_line.contains(&criterion.variable) {
            relevant_lines.insert(max_line);
        }
    }

    // Percorre de trás para frente a partir da linha do critério.
    for idx in (0..max_line).rev() {
        let line_text = lines[idx];
        let (assigned, used) = crate::slicer::parse_line(line_text);

        if let Some(ref var) = assigned {
            if relevant_vars.contains(var) {
                // Esta linha atribui a uma variável relevante.
                relevant_lines.insert(idx + 1); // 1-based
                                                // As variáveis usadas no lado direito agora se tornam relevantes.
                for u in used {
                    relevant_vars.insert(u);
                }
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
    fn test_backward_slice_simple() {
        let source = "let x = 1;\nlet y = x + 2;\nreturn y;";
        let criterion = SliceCriterion::new(3, "y");
        let result = backward_slice(source, &criterion).unwrap();
        // backward: linha 3 (critério), linha 2 (y = x + 2) e linha 1 (x = 1)
        assert_eq!(result.relevant_lines, vec![1, 2, 3]);
    }

    #[test]
    fn test_backward_slice_with_intermediates() {
        let source = "let a = 1;\nlet b = a + 2;\nlet c = b + 3;\nreturn c;";
        let criterion = SliceCriterion::new(4, "c");
        let result = backward_slice(source, &criterion).unwrap();
        // backward: linha 4 (critério), 3 (c = b + 3), 2 (b = a + 2), 1 (a = 1)
        assert_eq!(result.relevant_lines, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_backward_slice_empty_source() {
        let source = "";
        let criterion = SliceCriterion::new(1, "x");
        let result = backward_slice(source, &criterion).unwrap();
        assert!(result.relevant_lines.is_empty());
    }

    #[test]
    fn test_backward_slice_nonexistent_variable() {
        let source = "let x = 1;\nlet y = 2;";
        let criterion = SliceCriterion::new(1, "z");
        let result = backward_slice(source, &criterion).unwrap();
        // Como z não é atribuído em lugar nenhum, nenhuma linha é relevante.
        assert!(result.relevant_lines.is_empty());
    }

    #[test]
    fn test_backward_slice_preserves_order() {
        let source = "let m = 5;\nlet n = m;\nlet o = n;\nlet p = o;";
        let criterion = SliceCriterion::new(4, "p");
        let result = backward_slice(source, &criterion).unwrap();
        assert_eq!(result.relevant_lines, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_backward_slice_multi_block() {
        let source = "fn foo() {\n    let a = 1;\n    let b = a + 2;\n    if b > 0 {\n        let c = b;\n        return c;\n    }\n    return 0;\n}";
        let criterion = SliceCriterion::new(6, "c");
        let result = backward_slice(source, &criterion).unwrap();
        // c é atribuído na linha 6, usa b (linha 3 é b = a + 2, 2 é a = 1)
        assert_eq!(result.relevant_lines, vec![2, 3, 5, 6]);
    }
}
