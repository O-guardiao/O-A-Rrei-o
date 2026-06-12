//! BriefGenerator — geração determinística de PROJECT_BRIEF (PVC-Q3.3).
//!
//! Papel do "Arquiteto" no Self-Commissioning: transforma uma entrada
//! estruturada e validada em um PROJECT_BRIEF.md no formato dos briefs PVC
//! existentes (problema, escopo, métricas, dependências, riscos).
//! Sem LLM: a entrada vem do harness/operador; o gerador apenas valida
//! e renderiza. Briefs vazios ou sem problema definido são rejeitados
//! (gate G0 exige intenção explícita).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Métrica de sucesso com meta verificável.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessMetric {
    pub metric: String,
    pub target: String,
}

/// Risco com mitigação.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskItem {
    pub risk: String,
    pub mitigation: String,
}

/// Entrada estruturada do brief.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefInput {
    /// Identificador do PVC (ex.: "PVC-Q4.1").
    pub pvc_id: String,
    pub title: String,
    pub owner: String,
    /// Data ISO (yyyy-mm-dd) — fornecida pelo chamador para determinismo.
    pub date: String,
    pub problem: String,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub metrics: Vec<SuccessMetric>,
    pub dependencies: Vec<String>,
    pub risks: Vec<RiskItem>,
}

/// Gerador de PROJECT_BRIEF.
pub struct BriefGenerator;

impl BriefGenerator {
    /// Valida e renderiza o brief em Markdown (formato dos briefs PVC).
    pub fn render(input: &BriefInput) -> Result<String> {
        if input.pvc_id.trim().is_empty() || input.title.trim().is_empty() {
            bail!("brief requer pvc_id e title não vazios");
        }
        if input.problem.trim().is_empty() {
            bail!("brief sem problema definido — gate G0 exige intenção explícita");
        }
        if input.in_scope.is_empty() {
            bail!("brief sem escopo — defina ao menos um item dentro do escopo");
        }

        let mut md = String::new();
        md.push_str(&format!(
            "# PROJECT_BRIEF — {}: {}\n\n> **PVC:** {}\n> **Data:** {}\n> **Dono:** {}\n> **Status:** Proposto (gerado por Self-Commissioning)\n\n---\n\n",
            input.pvc_id, input.title, input.pvc_id, input.date, input.owner
        ));

        md.push_str("## 1. Problema\n\n");
        md.push_str(input.problem.trim());
        md.push_str("\n\n## 2. Escopo\n\n### Dentro do escopo\n");
        for item in &input.in_scope {
            md.push_str(&format!("- {}\n", item));
        }
        md.push_str("\n### Fora do escopo\n");
        if input.out_of_scope.is_empty() {
            md.push_str("- (nenhum item declarado)\n");
        } else {
            for item in &input.out_of_scope {
                md.push_str(&format!("- {}\n", item));
            }
        }

        md.push_str("\n## 3. Métricas de Sucesso\n\n| Métrica | Meta |\n|---|---|\n");
        if input.metrics.is_empty() {
            md.push_str("| (a definir) | (a definir) |\n");
        } else {
            for m in &input.metrics {
                md.push_str(&format!("| {} | {} |\n", m.metric, m.target));
            }
        }

        md.push_str("\n## 4. Dependências\n\n");
        if input.dependencies.is_empty() {
            md.push_str("- Nenhuma dependência declarada.\n");
        } else {
            for d in &input.dependencies {
                md.push_str(&format!("- {}\n", d));
            }
        }

        md.push_str("\n## 5. Riscos Principais\n\n| Risco | Mitigação |\n|---|---|\n");
        if input.risks.is_empty() {
            md.push_str("| (a avaliar) | (a avaliar) |\n");
        } else {
            for r in &input.risks {
                md.push_str(&format!("| {} | {} |\n", r.risk, r.mitigation));
            }
        }

        md.push_str("\n---\n\n*Brief gerado automaticamente pelo arreio-commissioning. Requer aprovação humana antes do gate G1.*\n");
        Ok(md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> BriefInput {
        BriefInput {
            pvc_id: "PVC-Q4.1".into(),
            title: "Exemplo".into(),
            owner: "@maintainer".into(),
            date: "2026-06-11".into(),
            problem: "O sistema não faz X.".into(),
            in_scope: vec!["Implementar X".into()],
            out_of_scope: vec!["Y fica de fora".into()],
            metrics: vec![SuccessMetric {
                metric: "Testes".into(),
                target: ">= 10".into(),
            }],
            dependencies: vec!["PVC-Q3.1".into()],
            risks: vec![RiskItem {
                risk: "Escopo crescer".into(),
                mitigation: "Gate G5".into(),
            }],
        }
    }

    #[test]
    fn renderiza_todas_as_secoes() {
        let md = BriefGenerator::render(&sample_input()).unwrap();
        assert!(md.contains("# PROJECT_BRIEF — PVC-Q4.1: Exemplo"));
        assert!(md.contains("## 1. Problema"));
        assert!(md.contains("## 2. Escopo"));
        assert!(md.contains("## 3. Métricas de Sucesso"));
        assert!(md.contains("## 4. Dependências"));
        assert!(md.contains("## 5. Riscos Principais"));
        assert!(md.contains("| Testes | >= 10 |"));
        assert!(md.contains("Requer aprovação humana"));
    }

    #[test]
    fn rejeita_problema_vazio() {
        let mut input = sample_input();
        input.problem = "   ".into();
        assert!(BriefGenerator::render(&input).is_err());
    }

    #[test]
    fn rejeita_escopo_vazio() {
        let mut input = sample_input();
        input.in_scope.clear();
        assert!(BriefGenerator::render(&input).is_err());
    }

    #[test]
    fn rejeita_pvc_id_vazio() {
        let mut input = sample_input();
        input.pvc_id = "".into();
        assert!(BriefGenerator::render(&input).is_err());
    }

    #[test]
    fn campos_opcionais_vazios_tem_placeholder_visivel() {
        let mut input = sample_input();
        input.metrics.clear();
        input.dependencies.clear();
        input.risks.clear();
        input.out_of_scope.clear();
        let md = BriefGenerator::render(&input).unwrap();
        assert!(md.contains("(a definir)"));
        assert!(md.contains("Nenhuma dependência declarada"));
        assert!(md.contains("(a avaliar)"));
        assert!(md.contains("(nenhum item declarado)"));
    }

    #[test]
    fn render_e_deterministico() {
        let input = sample_input();
        let a = BriefGenerator::render(&input).unwrap();
        let b = BriefGenerator::render(&input).unwrap();
        assert_eq!(a, b);
    }
}
