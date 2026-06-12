use crate::{RefinementLaw, RefinementStep};
use crate::specification::SpecificationStatement;
use crate::refinement_engine::RefinementEngine;
use anyhow::Result;

/// Gerador de código Rust a partir de passos de refinamento.
pub struct CodeGenerator;

impl CodeGenerator {
    /// Gera código Rust a partir de uma sequência de passos de refinamento.
    pub fn generate(steps: Vec<RefinementStep>) -> Result<String> {
        let mut lines = Vec::new();
        for step in steps {
            let code = Self::generate_law(&step.law, 0)?;
            if !code.is_empty() {
                lines.push(code);
            }
        }
        Ok(lines.join("\n"))
    }

    fn generate_law(law: &RefinementLaw, indent: usize) -> Result<String> {
        let pad = "    ".repeat(indent);
        match law {
            RefinementLaw::Assignment { var, expr } => {
                Ok(format!("{}let mut {} = {};", pad, var, expr))
            }
            RefinementLaw::IntroduceLocal { name, init } => {
                Ok(format!("{}let {} = {};", pad, name, init))
            }
            RefinementLaw::Alternation {
                condition,
                then_branch,
                else_branch,
            } => {
                let mut out = format!("{}if {} {{\n", pad, condition);
                out.push_str(&Self::generate_law(then_branch, indent + 1)?);
                out.push_str(&format!("\n{}}} else {{\n", pad));
                out.push_str(&Self::generate_law(else_branch, indent + 1)?);
                out.push_str(&format!("\n{}}}", pad));
                Ok(out)
            }
            RefinementLaw::Iteration {
                condition,
                invariant,
                body,
            } => {
                let mut out = format!("{}// invariant: {}\n", pad, invariant);
                out.push_str(&format!("{}while {} {{\n", pad, condition));
                out.push_str(&Self::generate_law(body, indent + 1)?);
                out.push_str(&format!("\n{}}}", pad));
                Ok(out)
            }
            RefinementLaw::SequentialComposition { first, second } => {
                let mut out = Self::generate_law(first, indent)?;
                let second_code = Self::generate_law(second, indent)?;
                if !second_code.is_empty() {
                    out.push('\n');
                    out.push_str(&second_code);
                }
                Ok(out)
            }
            RefinementLaw::WeakenPrecondition { .. }
            | RefinementLaw::StrengthenPostcondition { .. } => {
                // Leis de especificação não geram código executável diretamente.
                Ok(String::new())
            }
        }
    }
}

/// Pipeline de conveniência: gera código a partir de especificação + target.
/// Usa refinamento automático orientado por `target`.
pub fn generate_from_spec(spec: &SpecificationStatement, target: &str) -> Result<String> {
    let mut engine = RefinementEngine::new(spec.clone());
    engine.auto_refine(target)?;
    CodeGenerator::generate(engine.trace())
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RefinementLaw, SpecificationStatement};

    #[test]
    fn generate_assignment() {
        let spec = SpecificationStatement::new(
            vec!["x".to_string()],
            "x >= 0",
            "x == 42",
        );
        let code = generate_from_spec(&spec, "x == 42").unwrap();
        assert!(code.contains("let mut x = 42;"));
    }

    #[test]
    fn generate_introduce_local() {
        let spec = SpecificationStatement::new(
            vec!["result".to_string()],
            "true",
            "result == a + b",
        );
        let mut engine = RefinementEngine::new(spec);
        engine
            .refine(RefinementLaw::IntroduceLocal {
                name: "temp".to_string(),
                init: "a + b".to_string(),
            })
            .unwrap();
        engine
            .refine(RefinementLaw::Assignment {
                var: "result".to_string(),
                expr: "temp".to_string(),
            })
            .unwrap();
        let code = CodeGenerator::generate(engine.trace()).unwrap();
        assert!(code.contains("let temp = a + b;"));
        assert!(code.contains("let mut result = temp;"));
    }

    #[test]
    fn generate_alternation() {
        let spec = SpecificationStatement::new(
            vec!["x".to_string()],
            "true",
            "x >= 0",
        );
        let mut engine = RefinementEngine::new(spec);
        engine
            .refine(RefinementLaw::Alternation {
                condition: "input > 0".to_string(),
                then_branch: Box::new(RefinementLaw::Assignment {
                    var: "x".to_string(),
                    expr: "input".to_string(),
                }),
                else_branch: Box::new(RefinementLaw::Assignment {
                    var: "x".to_string(),
                    expr: "0".to_string(),
                }),
            })
            .unwrap();
        let code = CodeGenerator::generate(engine.trace()).unwrap();
        assert!(code.contains("if input > 0 {"));
        assert!(code.contains("} else {"));
        assert!(code.contains("let mut x = input;"));
        assert!(code.contains("let mut x = 0;"));
    }

    #[test]
    fn generate_empty_steps() {
        let code = CodeGenerator::generate(Vec::new()).unwrap();
        assert!(code.is_empty());
    }

    #[test]
    fn generate_weaken_precondition_skipped() {
        let spec = SpecificationStatement::new(vec![], "true", "true");
        let mut engine = RefinementEngine::new(spec);
        engine
            .refine(RefinementLaw::WeakenPrecondition {
                new_pre: "true".to_string(),
            })
            .unwrap();
        let code = CodeGenerator::generate(engine.trace()).unwrap();
        // Weaken/Strengthen não geram código
        assert!(code.is_empty());
    }
}
