//! ReportGenerator — geração de COMMISSIONING_REPORT a partir de evidências (PVC-Q3.3).
//!
//! Papel do "Refiner" no Self-Commissioning: consolida evidências REAIS
//! (saída de `cargo test`, varredura de stubs, fluxos verificados) em um
//! relatório de comissionamento no formato PVC. A decisão final é calculada
//! deterministicamente a partir das evidências — nunca declarada sem prova:
//! - qualquer falha de teste ou de fluxo → **Reprovado**;
//! - stubs de alta severidade, pendências ou restrições → **Aprovado com restrições**;
//! - caso contrário → **Aprovado**.

use crate::stub_detector::StubReport;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Resumo de execução de testes (evidência primária).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    /// Quantos blocos "test result:" foram agregados.
    pub suites: u32,
}

impl TestSummary {
    /// Extrai o resumo da saída textual real de `cargo test`.
    /// Linhas esperadas: `test result: ok. 53 passed; 0 failed; 1 ignored; ...`
    pub fn parse_cargo_test_output(output: &str) -> Self {
        let mut summary = TestSummary::default();
        for line in output.lines() {
            let line = line.trim();
            if !line.starts_with("test result:") {
                continue;
            }
            summary.suites += 1;
            for part in line.split(';') {
                let part = part.trim();
                if let Some(n) = parse_count(part, "passed") {
                    summary.passed += n;
                } else if let Some(n) = parse_count(part, "failed") {
                    summary.failed += n;
                } else if let Some(n) = parse_count(part, "ignored") {
                    summary.ignored += n;
                }
            }
        }
        summary
    }
}

/// Extrai "N <label>" de um fragmento como "test result: ok. 53 passed".
fn parse_count(fragment: &str, label: &str) -> Option<u32> {
    if !fragment.ends_with(label) {
        return None;
    }
    fragment
        .trim_end_matches(label)
        .trim()
        .split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
}

/// Evidência de um fluxo comissionado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowEvidence {
    pub id: String,
    pub action: String,
    pub expected: String,
    pub observed: String,
    pub passed: bool,
}

/// Decisão calculada do comissionamento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommissioningDecision {
    Aprovado,
    AprovadoComRestricoes,
    Reprovado,
}

impl CommissioningDecision {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Aprovado => "✅ Aprovado",
            Self::AprovadoComRestricoes => "✅ Aprovado com restrições",
            Self::Reprovado => "❌ Reprovado",
        }
    }
}

/// Pacote de evidências para o relatório.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePack {
    pub system: String,
    pub version: String,
    /// Data ISO fornecida pelo chamador (determinismo).
    pub date: String,
    pub environment: String,
    pub flows: Vec<FlowEvidence>,
    pub tests: TestSummary,
    pub stubs: Option<StubReport>,
    pub pending: Vec<String>,
    pub restrictions: Vec<String>,
}

/// Gerador do relatório de comissionamento.
pub struct ReportGenerator;

impl ReportGenerator {
    /// Decisão determinística a partir das evidências.
    pub fn decide(pack: &EvidencePack) -> CommissioningDecision {
        let any_flow_failed = pack.flows.iter().any(|f| !f.passed);
        if any_flow_failed || pack.tests.failed > 0 {
            return CommissioningDecision::Reprovado;
        }
        let high_stubs = pack
            .stubs
            .as_ref()
            .map(|s| s.high_severity_count)
            .unwrap_or(0);
        if high_stubs > 0 || !pack.pending.is_empty() || !pack.restrictions.is_empty() {
            return CommissioningDecision::AprovadoComRestricoes;
        }
        CommissioningDecision::Aprovado
    }

    /// Renderiza o COMMISSIONING_REPORT.md no formato PVC.
    /// Exige ao menos uma fonte de evidência (testes ou fluxos) — relatório
    /// sem evidência viola a regra PVC e é rejeitado.
    pub fn render(pack: &EvidencePack) -> Result<String> {
        if pack.flows.is_empty() && pack.tests.suites == 0 {
            bail!("relatório sem evidências: forneça fluxos verificados ou saída de testes");
        }

        let decision = Self::decide(pack);
        let mut md = String::new();

        md.push_str(&format!(
            "# COMMISSIONING_REPORT — {}\n\n> **Data**: {}\n> **Versão**: {}\n> **Ambiente**: {}\n> **Origem**: gerado por arreio-commissioning (Self-Commissioning, PVC-Q3.3)\n\n---\n\n",
            pack.system, pack.date, pack.version, pack.environment
        ));

        md.push_str("## 1. Evidência de Testes\n\n| Métrica | Valor |\n|---|---|\n");
        md.push_str(&format!("| Testes passando | {} |\n", pack.tests.passed));
        md.push_str(&format!("| Testes falhando | {} |\n", pack.tests.failed));
        md.push_str(&format!("| Testes ignorados | {} |\n", pack.tests.ignored));
        md.push_str(&format!("| Suítes agregadas | {} |\n", pack.tests.suites));

        md.push_str("\n## 2. Fluxos Comissionados\n\n");
        if pack.flows.is_empty() {
            md.push_str("Nenhum fluxo manual registrado (evidência via testes).\n");
        } else {
            md.push_str("| Passo | Ação | Esperado | Observado | Status |\n|---|---|---|---|---|\n");
            for f in &pack.flows {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    f.id,
                    f.action,
                    f.expected,
                    f.observed,
                    if f.passed { "✅" } else { "❌" }
                ));
            }
        }

        md.push_str("\n## 3. Stubs e Incompletudes (regra: incompleto oculto não pode)\n\n");
        match &pack.stubs {
            None => md.push_str("Varredura de stubs não executada nesta rodada.\n"),
            Some(report) => {
                md.push_str(&format!(
                    "Arquivos varridos: {} | Alta severidade (todo!/unimplemented!): {} | Baixa severidade (TODO/FIXME): {}\n",
                    report.files_scanned, report.high_severity_count, report.low_severity_count
                ));
                if !report.findings.is_empty() {
                    md.push_str("\n| Arquivo | Linha | Tipo |\n|---|---|---|\n");
                    for f in report.findings.iter().take(50) {
                        md.push_str(&format!("| {} | {} | {:?} |\n", f.file, f.line, f.kind));
                    }
                    if report.findings.len() > 50 {
                        md.push_str(&format!(
                            "\n_(+{} ocorrências omitidas do relatório — íntegra na varredura)_\n",
                            report.findings.len() - 50
                        ));
                    }
                }
            }
        }

        md.push_str("\n## 4. Pendências\n\n");
        if pack.pending.is_empty() {
            md.push_str("Nenhuma pendência registrada.\n");
        } else {
            for p in &pack.pending {
                md.push_str(&format!("- {}\n", p));
            }
        }

        md.push_str("\n## 5. Restrições\n\n");
        if pack.restrictions.is_empty() {
            md.push_str("Nenhuma restrição registrada.\n");
        } else {
            for r in &pack.restrictions {
                md.push_str(&format!("- {}\n", r));
            }
        }

        md.push_str(&format!(
            "\n## 6. Decisão\n\n**{}** — decisão calculada deterministicamente a partir das evidências acima.\n\n---\n\n*Relatório gerado automaticamente pelo arreio-commissioning. Baseado exclusivamente em evidências verificáveis.*\n",
            decision.label()
        ));

        Ok(md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_pack() -> EvidencePack {
        EvidencePack {
            system: "O Arreio".into(),
            version: "4.7".into(),
            date: "2026-06-11".into(),
            environment: "Windows 11 / GNU".into(),
            flows: vec![FlowEvidence {
                id: "1.1".into(),
                action: "cargo check".into(),
                expected: "0 erros".into(),
                observed: "0 erros".into(),
                passed: true,
            }],
            tests: TestSummary {
                passed: 100,
                failed: 0,
                ignored: 0,
                suites: 3,
            },
            stubs: None,
            pending: vec![],
            restrictions: vec![],
        }
    }

    #[test]
    fn parse_cargo_test_output_agrega_suites() {
        let output = "\
running 5 tests
test result: ok. 53 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 4.61s
outra linha qualquer
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
test result: FAILED. 36 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let summary = TestSummary::parse_cargo_test_output(output);
        assert_eq!(summary.suites, 3);
        assert_eq!(summary.passed, 114);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.ignored, 1);
    }

    #[test]
    fn decisao_aprovado_quando_tudo_verde() {
        assert_eq!(
            ReportGenerator::decide(&base_pack()),
            CommissioningDecision::Aprovado
        );
    }

    #[test]
    fn decisao_reprovado_com_teste_falhando() {
        let mut pack = base_pack();
        pack.tests.failed = 1;
        assert_eq!(
            ReportGenerator::decide(&pack),
            CommissioningDecision::Reprovado
        );
    }

    #[test]
    fn decisao_reprovado_com_fluxo_falhando() {
        let mut pack = base_pack();
        pack.flows[0].passed = false;
        assert_eq!(
            ReportGenerator::decide(&pack),
            CommissioningDecision::Reprovado
        );
    }

    #[test]
    fn decisao_restricoes_com_stub_de_alta_severidade() {
        let mut pack = base_pack();
        pack.stubs = Some(crate::stub_detector::StubReport {
            files_scanned: 10,
            findings: vec![],
            high_severity_count: 2,
            low_severity_count: 0,
        });
        assert_eq!(
            ReportGenerator::decide(&pack),
            CommissioningDecision::AprovadoComRestricoes
        );
    }

    #[test]
    fn decisao_restricoes_com_pendencias() {
        let mut pack = base_pack();
        pack.pending.push("IDE integration não testada".into());
        assert_eq!(
            ReportGenerator::decide(&pack),
            CommissioningDecision::AprovadoComRestricoes
        );
    }

    #[test]
    fn render_contem_secoes_e_decisao() {
        let md = ReportGenerator::render(&base_pack()).unwrap();
        assert!(md.contains("## 1. Evidência de Testes"));
        assert!(md.contains("## 2. Fluxos Comissionados"));
        assert!(md.contains("## 6. Decisão"));
        assert!(md.contains("✅ Aprovado"));
        assert!(md.contains("| 1.1 | cargo check |"));
    }

    #[test]
    fn render_sem_evidencia_e_rejeitado() {
        let mut pack = base_pack();
        pack.flows.clear();
        pack.tests = TestSummary::default();
        assert!(ReportGenerator::render(&pack).is_err());
    }

    #[test]
    fn render_lista_stubs_no_relatorio() {
        let mut pack = base_pack();
        pack.stubs = Some(crate::stub_detector::StubReport {
            files_scanned: 3,
            findings: vec![crate::stub_detector::StubFinding {
                file: "src/x.rs".into(),
                line: 7,
                kind: crate::stub_detector::StubKind::TodoMacro,
                snippet: "todo!()".into(),
            }],
            high_severity_count: 1,
            low_severity_count: 0,
        });
        let md = ReportGenerator::render(&pack).unwrap();
        assert!(md.contains("| src/x.rs | 7 | TodoMacro |"));
        assert!(md.contains("Aprovado com restrições"));
    }
}
