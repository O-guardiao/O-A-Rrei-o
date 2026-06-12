//! # arreio-refinement
//!
//! Refinement-Based Generation — geração de código via refinamento formal.
//!
//! Especificação → Refinamento → Código, com preservação de garantias em cada passo.
//!
//! Inspirado em: Back, Morgan, Morris (1980s).

pub mod generator;
pub mod refinement_engine;
pub mod refinement_law;
pub mod specification;

pub use generator::*;
pub use refinement_engine::*;
pub use refinement_law::*;
pub use specification::*;

/// Passo individual de refinamento: de uma especificação, aplicando uma lei,
/// para uma nova especificação.
#[derive(Debug, Clone, PartialEq)]
pub struct RefinementStep {
    pub from: crate::specification::SpecificationStatement,
    pub law: crate::refinement_law::RefinementLaw,
    pub to: crate::specification::SpecificationStatement,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_construction_and_validation() {
        let spec = SpecificationStatement::new(vec!["x".to_string()], "x > 0", "x == 5");
        assert_eq!(spec.frame, vec!["x"]);
        assert_eq!(spec.pre, "x > 0");
        assert_eq!(spec.post, "x == 5");
        assert_eq!(spec.notation(), "w : [x > 0, x == 5]");
        assert!(spec.is_satisfied_by("let mut x = 5;"));
        assert!(!spec.is_satisfied_by(""));
    }

    #[test]
    fn weaken_precondition() {
        let spec = SpecificationStatement::new(vec![], "x > 0", "true");
        let law = RefinementLaw::WeakenPrecondition {
            new_pre: "true".to_string(),
        };
        let step = law.apply(&spec).unwrap();
        assert_eq!(step.to.pre, "true");
        assert_eq!(step.to.post, "true");
    }

    #[test]
    fn strengthen_postcondition() {
        let spec = SpecificationStatement::new(vec![], "true", "x > 0");
        let law = RefinementLaw::StrengthenPostcondition {
            new_post: "x > 5".to_string(),
        };
        let step = law.apply(&spec).unwrap();
        assert_eq!(step.to.post, "x > 5");
        assert_eq!(step.to.pre, "true");
    }

    #[test]
    fn assignment_generates_code() {
        let step = RefinementStep {
            from: SpecificationStatement::new(vec!["x".to_string()], "true", "x == 5"),
            law: RefinementLaw::Assignment {
                var: "x".to_string(),
                expr: "5".to_string(),
            },
            to: SpecificationStatement::new(vec!["x".to_string()], "true", "true"),
        };
        let code = CodeGenerator::generate(vec![step]).unwrap();
        assert!(code.contains("let mut x = 5;"));
    }

    #[test]
    fn sequential_composition_chains_two_steps() {
        let spec = SpecificationStatement::new(
            vec!["x".to_string(), "y".to_string()],
            "true",
            "y == x + 1",
        );
        let law = RefinementLaw::SequentialComposition {
            first: Box::new(RefinementLaw::Assignment {
                var: "x".to_string(),
                expr: "5".to_string(),
            }),
            second: Box::new(RefinementLaw::Assignment {
                var: "y".to_string(),
                expr: "x + 1".to_string(),
            }),
        };
        let step = law.apply(&spec).unwrap();
        assert_eq!(step.to.post, "(x + 1) == (5) + 1");
    }

    #[test]
    fn alternation_generates_if_else() {
        let step = RefinementStep {
            from: SpecificationStatement::new(
                vec!["x".to_string(), "y".to_string()],
                "true",
                "true",
            ),
            law: RefinementLaw::Alternation {
                condition: "x > 0".to_string(),
                then_branch: Box::new(RefinementLaw::Assignment {
                    var: "y".to_string(),
                    expr: "x".to_string(),
                }),
                else_branch: Box::new(RefinementLaw::Assignment {
                    var: "y".to_string(),
                    expr: "0".to_string(),
                }),
            },
            to: SpecificationStatement::new(vec!["x".to_string(), "y".to_string()], "true", "true"),
        };
        let code = CodeGenerator::generate(vec![step]).unwrap();
        assert!(code.contains("if x > 0 {"));
        assert!(code.contains("} else {"));
        assert!(code.contains("let mut y = x;"));
        assert!(code.contains("let mut y = 0;"));
    }

    #[test]
    fn iteration_generates_while_with_invariant() {
        let step = RefinementStep {
            from: SpecificationStatement::new(
                vec!["i".to_string(), "n".to_string()],
                "true",
                "true",
            ),
            law: RefinementLaw::Iteration {
                condition: "i < n".to_string(),
                invariant: "i >= 0".to_string(),
                body: Box::new(RefinementLaw::Assignment {
                    var: "i".to_string(),
                    expr: "i + 1".to_string(),
                }),
            },
            to: SpecificationStatement::new(vec!["i".to_string(), "n".to_string()], "true", "true"),
        };
        let code = CodeGenerator::generate(vec![step]).unwrap();
        assert!(code.contains("while i < n {"));
        assert!(code.contains("// invariant: i >= 0"));
        assert!(code.contains("let mut i = i + 1;"));
        assert!(code.contains("}"));
    }

    #[test]
    fn refinement_engine_full_trace() {
        let spec = SpecificationStatement::new(vec!["x".to_string()], "true", "x > 0");
        let mut engine = RefinementEngine::new(spec.clone());
        engine
            .refine(RefinementLaw::WeakenPrecondition {
                new_pre: "true".to_string(),
            })
            .unwrap();
        engine
            .refine(RefinementLaw::StrengthenPostcondition {
                new_post: "x == 5".to_string(),
            })
            .unwrap();
        let trace = engine.trace();
        assert_eq!(trace.len(), 2);
        assert_eq!(
            trace[0].law,
            RefinementLaw::WeakenPrecondition {
                new_pre: "true".to_string()
            }
        );
        assert_eq!(
            trace[1].law,
            RefinementLaw::StrengthenPostcondition {
                new_post: "x == 5".to_string()
            }
        );
        assert_eq!(trace[0].from, spec);
        assert_eq!(trace[1].from, trace[0].to);
    }

    #[test]
    fn auto_refine_reaches_simple_target() {
        let spec = SpecificationStatement::new(vec!["x".to_string()], "true", "true");
        let mut engine = RefinementEngine::new(spec);
        engine.auto_refine("x == 5").unwrap();
        assert_eq!(engine.current.post, "x == 5");
        let trace = engine.trace();
        assert!(!trace.is_empty());
        let last = trace.last().unwrap();
        assert!(matches!(
            last.law,
            RefinementLaw::StrengthenPostcondition { .. }
        ));
    }

    #[test]
    fn generated_code_is_syntactically_valid_rust() {
        let steps = vec![
            RefinementStep {
                from: SpecificationStatement::new(vec!["x".to_string()], "true", "true"),
                law: RefinementLaw::IntroduceLocal {
                    name: "x".to_string(),
                    init: "0".to_string(),
                },
                to: SpecificationStatement::new(vec!["x".to_string()], "true", "true"),
            },
            RefinementStep {
                from: SpecificationStatement::new(vec!["x".to_string()], "true", "true"),
                law: RefinementLaw::Alternation {
                    condition: "x > 0".to_string(),
                    then_branch: Box::new(RefinementLaw::Assignment {
                        var: "x".to_string(),
                        expr: "x - 1".to_string(),
                    }),
                    else_branch: Box::new(RefinementLaw::Assignment {
                        var: "x".to_string(),
                        expr: "0".to_string(),
                    }),
                },
                to: SpecificationStatement::new(vec!["x".to_string()], "true", "true"),
            },
        ];
        let code = CodeGenerator::generate(steps).unwrap();
        let wrapped = format!("{{\n{}\n}}", code);
        syn::parse_str::<syn::Block>(&wrapped)
            .expect("código gerado deve ser sintaticamente válido");
    }
}
