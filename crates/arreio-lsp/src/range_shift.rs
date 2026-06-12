/// Calcula o mapeamento de linhas entre pre_text e post_text.
/// Inspirado no difflib.SequenceMatcher do Hermes, portado para Rust.
pub fn build_line_shift(pre_text: &str, post_text: &str) -> LineShiftMap {
    let pre_lines: Vec<&str> = pre_text.lines().collect();
    let post_lines: Vec<&str> = post_text.lines().collect();

    let mut shifts = Vec::new();
    let mut pre_idx = 0usize;
    let mut post_idx = 0usize;

    // Algoritmo simples de diff: encontra matches e registra shifts
    while pre_idx < pre_lines.len() && post_idx < post_lines.len() {
        if pre_lines[pre_idx] == post_lines[post_idx] {
            // Linha igual — mapeamento 1:1
            shifts.push((pre_idx + 1, post_idx + 1, 0i32)); // line numbers 1-based, shift 0
            pre_idx += 1;
            post_idx += 1;
        } else {
            // Tenta encontrar a próxima linha igual no post
            let mut found = false;
            for look_ahead in 1..=5 {
                if post_idx + look_ahead < post_lines.len()
                    && pre_lines[pre_idx] == post_lines[post_idx + look_ahead]
                {
                    // Linhas inseridas antes
                    for _ in 0..look_ahead {
                        shifts.push((pre_idx + 1, post_idx + 1, look_ahead as i32));
                    }
                    post_idx += look_ahead;
                    found = true;
                    break;
                }
                if pre_idx + look_ahead < pre_lines.len()
                    && pre_lines[pre_idx + look_ahead] == post_lines[post_idx]
                {
                    // Linhas removidas
                    for _ in 0..look_ahead {
                        shifts.push((pre_idx + 1, post_idx + 1, -(look_ahead as i32)));
                        pre_idx += 1;
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                // Não encontrou — assume substituição
                shifts.push((pre_idx + 1, post_idx + 1, 0));
                pre_idx += 1;
                post_idx += 1;
            }
        }
    }

    LineShiftMap { shifts }
}

#[derive(Debug, Clone)]
pub struct LineShiftMap {
    shifts: Vec<(usize, usize, i32)>, // (pre_line, post_line, shift)
}

impl LineShiftMap {
    /// Aplica o shift a uma linha do baseline.
    pub fn shift_line(&self, pre_line: usize) -> Option<usize> {
        for (pl, post, shift) in &self.shifts {
            if *pl == pre_line {
                let shifted = (*post as i32 + shift) as usize;
                return Some(shifted.max(1));
            }
        }
        // Fallback: busca o shift mais próximo
        self.shifts
            .iter()
            .filter(|(pl, _, _)| *pl <= pre_line)
            .last()
            .map(|(pl, post, _)| {
                let offset = pre_line - pl;
                post + offset
            })
    }
}

/// Aplica shift_baseline antes do set-difference de diagnósticos.
pub fn shift_baseline(
    baseline: &mut Vec<crate::baseline_store::Diagnostic>,
    shift_map: &LineShiftMap,
) {
    for diag in baseline.iter_mut() {
        if let Some(new_line) = shift_map.shift_line(diag.line) {
            diag.line = new_line;
        }
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_shift_no_changes() {
        let text = "line1\nline2\nline3";
        let map = build_line_shift(text, text);
        assert_eq!(map.shift_line(1), Some(1));
        assert_eq!(map.shift_line(2), Some(2));
        assert_eq!(map.shift_line(3), Some(3));
    }

    #[test]
    fn build_shift_with_insertion() {
        let pre = "line1\nline2\nline3";
        let post = "line1\ninserted\nline2\nline3";
        let map = build_line_shift(pre, post);
        assert_eq!(map.shift_line(1), Some(1));
        // line2 do pre → line3 do post
        assert_eq!(map.shift_line(2), Some(3));
        assert_eq!(map.shift_line(3), Some(4));
    }

    #[test]
    fn build_shift_with_deletion() {
        let pre = "line1\nline2\nline3";
        let post = "line1\nline3";
        let map = build_line_shift(pre, post);
        assert_eq!(map.shift_line(1), Some(1));
        assert_eq!(map.shift_line(3), Some(2));
    }

    #[test]
    fn shift_baseline_updates_lines() {
        use crate::baseline_store::Diagnostic;
        let mut baseline = vec![Diagnostic {
            file: "a.rs".to_string(),
            line: 3,
            column: 1,
            severity: "ERROR".to_string(),
            message: "error".to_string(),
            code: None,
            source: None,
        }];
        let pre = "line1\nline2\nline3";
        let post = "line1\ninserted\nline2\nline3";
        let map = build_line_shift(pre, post);
        shift_baseline(&mut baseline, &map);
        assert_eq!(baseline[0].line, 4); // line3 → line4 após inserção
    }
}
