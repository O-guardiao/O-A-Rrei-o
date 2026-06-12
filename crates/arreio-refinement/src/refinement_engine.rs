use crate::{RefinementLaw, RefinementStep, SpecificationStatement};
use anyhow::Result;

/// Motor de refinamento: aplica leis sistematicamente a uma especificação inicial.
pub struct RefinementEngine {
    pub steps: Vec<RefinementStep>,
    pub current: SpecificationStatement,
}

impl RefinementEngine {
    pub fn new(initial: SpecificationStatement) -> Self {
        Self {
            steps: Vec::new(),
            current: initial,
        }
    }

    /// Aplica uma lei de refinamento ao estado atual.
    pub fn refine(&mut self, law: RefinementLaw) -> Result<()> {
        let step = law.apply(&self.current)?;
        self.current = step.to.clone();
        self.steps.push(step);
        Ok(())
    }

    /// Aplica leis automaticamente até atingir o target desejado.
    pub fn auto_refine(&mut self, target: &str) -> Result<()> {
        // Heurística 1: target é igualdade "var == expr" com variável no frame
        if let Some((var, expr)) = parse_equality(target) {
            if self.current.frame.contains(&var) {
                self.refine(RefinementLaw::Assignment { var, expr })?;
                if self.current.post != target {
                    self.refine(RefinementLaw::StrengthenPostcondition {
                        new_post: target.to_string(),
                    })?;
                }
                return Ok(());
            }
        }

        // Heurística 2: target é atribuição simples "var = expr"
        if let Some((var, expr)) = parse_assignment(target) {
            if self.current.frame.contains(&var) {
                self.refine(RefinementLaw::Assignment { var, expr })?;
                if self.current.post != target {
                    self.refine(RefinementLaw::StrengthenPostcondition {
                        new_post: target.to_string(),
                    })?;
                }
                return Ok(());
            }
        }

        // Heurística 3: fortalecer pós-condição diretamente para o target
        self.refine(RefinementLaw::StrengthenPostcondition {
            new_post: target.to_string(),
        })?;
        Ok(())
    }

    /// Retorna o histórico completo de refinamento.
    pub fn trace(&self) -> Vec<RefinementStep> {
        self.steps.clone()
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

fn parse_assignment(s: &str) -> Option<(String, String)> {
    let mut parts = s.splitn(2, '=');
    let var = parts.next()?.trim().to_string();
    let expr = parts.next()?.trim().to_string();
    if expr.starts_with('=') || expr.is_empty() {
        None
    } else {
        Some((var, expr))
    }
}
