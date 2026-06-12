use crate::{RefinementStep, SpecificationStatement};
use anyhow::{bail, Result};

/// Leis de refinamento formal.
/// Cada lei transforma uma especificação em uma especificação mais concreta.
#[derive(Debug, Clone, PartialEq)]
pub enum RefinementLaw {
    /// Enfraquece a pré-condição (torna o programa mais aplicável).
    WeakenPrecondition { new_pre: String },
    /// Fortalece a pós-condição (torna o programa mais preciso).
    StrengthenPostcondition { new_post: String },
    /// Introduz uma variável local no frame.
    IntroduceLocal { name: String, init: String },
    /// Atribuição: substitui a variável na pós-condição pela expressão.
    Assignment { var: String, expr: String },
    /// Composição sequencial de duas leis.
    SequentialComposition {
        first: Box<RefinementLaw>,
        second: Box<RefinementLaw>,
    },
    /// Alternativa (if/else): escolha baseada em condição.
    Alternation {
        condition: String,
        then_branch: Box<RefinementLaw>,
        else_branch: Box<RefinementLaw>,
    },
    /// Iteração (while): loop com invariante.
    Iteration {
        condition: String,
        invariant: String,
        body: Box<RefinementLaw>,
    },
}

impl RefinementLaw {
    /// Aplica a lei de refinamento a uma especificação, produzindo um passo de refinamento.
    pub fn apply(&self, spec: &SpecificationStatement) -> Result<RefinementStep> {
        let to = match self {
            RefinementLaw::WeakenPrecondition { new_pre } => SpecificationStatement {
                frame: spec.frame.clone(),
                pre: new_pre.clone(),
                post: spec.post.clone(),
            },
            RefinementLaw::StrengthenPostcondition { new_post } => SpecificationStatement {
                frame: spec.frame.clone(),
                pre: spec.pre.clone(),
                post: new_post.clone(),
            },
            RefinementLaw::IntroduceLocal { name, init } => {
                let mut new_frame = spec.frame.clone();
                new_frame.push(name.clone());
                let new_pre = format!("{} && {} == {}", spec.pre, name, init);
                SpecificationStatement {
                    frame: new_frame,
                    pre: new_pre,
                    post: spec.post.clone(),
                }
            }
            RefinementLaw::Assignment { var, expr } => {
                if !spec.frame.contains(var) {
                    bail!("variável '{}' não está no frame", var);
                }
                let new_post = spec.post.replace(var, &format!("({})", expr));
                SpecificationStatement {
                    frame: spec.frame.clone(),
                    pre: spec.pre.clone(),
                    post: new_post,
                }
            }
            RefinementLaw::SequentialComposition { first, second } => {
                let step1 = first.apply(spec)?;
                let step2 = second.apply(&step1.to)?;
                return Ok(RefinementStep {
                    from: spec.clone(),
                    law: self.clone(),
                    to: step2.to,
                });
            }
            RefinementLaw::Alternation {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_pre = format!("{} && {}", spec.pre, condition);
                let else_pre = format!("{} && !({})", spec.pre, condition);
                let then_spec =
                    SpecificationStatement::new(spec.frame.clone(), then_pre, spec.post.clone());
                let else_spec =
                    SpecificationStatement::new(spec.frame.clone(), else_pre, spec.post.clone());
                let then_step = then_branch.apply(&then_spec)?;
                let _else_step = else_branch.apply(&else_spec)?;
                // Usa o ramo then como resultado representativo
                SpecificationStatement {
                    frame: spec.frame.clone(),
                    pre: spec.pre.clone(),
                    post: then_step.to.post.clone(),
                }
            }
            RefinementLaw::Iteration {
                condition,
                invariant,
                body,
            } => {
                let body_pre = format!("{} && {}", invariant, condition);
                let body_spec =
                    SpecificationStatement::new(spec.frame.clone(), body_pre, invariant.clone());
                let _body_step = body.apply(&body_spec)?;
                // Após o loop: invariante mantida e condição é falsa
                let final_post = format!("{} && !({})", invariant, condition);
                SpecificationStatement {
                    frame: spec.frame.clone(),
                    pre: spec.pre.clone(),
                    post: final_post,
                }
            }
        };

        Ok(RefinementStep {
            from: spec.clone(),
            law: self.clone(),
            to,
        })
    }
}
