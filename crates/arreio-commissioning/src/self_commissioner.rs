//! SelfCommissioner — orquestrador do Meta-PVC (PVC-Q3.3).
//!
//! Fecha o ciclo: o próprio sistema produz seus artefatos PVC a partir de
//! evidências reais. Fluxo: varre stubs (Inspector) → consolida evidências
//! de teste (Refiner) → gera PROJECT_BRIEF e COMMISSIONING_REPORT → registra
//! a execução no Blackboard para auditoria. Tudo determinístico; a aprovação
//! final continua humana (HITL — os artefatos nascem marcados como gerados).

use crate::brief_generator::{BriefGenerator, BriefInput};
use crate::report_generator::{CommissioningDecision, EvidencePack, ReportGenerator};
use crate::stub_detector::{StubDetector, StubReport};
use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Artefatos produzidos por uma rodada de self-commissioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommissioningArtifacts {
    pub brief_md: Option<String>,
    pub report_md: String,
    pub stub_report: StubReport,
    pub decision: CommissioningDecision,
}

/// Orquestrador do self-commissioning.
pub struct SelfCommissioner {
    blackboard: Blackboard,
    detector: StubDetector,
}

impl SelfCommissioner {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            detector: StubDetector::new(),
        }
    }

    pub fn with_detector(mut self, detector: StubDetector) -> Self {
        self.detector = detector;
        self
    }

    /// Executa a rodada completa:
    /// 1. varre `source_root` por stubs (a varredura entra como evidência);
    /// 2. mescla o resultado no `EvidencePack` fornecido pelo chamador;
    /// 3. gera o COMMISSIONING_REPORT (e o PROJECT_BRIEF, se `brief` vier);
    /// 4. publica o resumo auditável no Blackboard (`commissioning::last_run`).
    pub fn commission(
        &self,
        source_root: &Path,
        mut evidence: EvidencePack,
        brief: Option<&BriefInput>,
    ) -> Result<CommissioningArtifacts> {
        // 1. Inspector: varredura de stubs.
        let stub_report = self
            .detector
            .scan(source_root)
            .context("varredura de stubs falhou")?;
        evidence.stubs = Some(stub_report.clone());

        // 2/3. Refiner: relatório a partir das evidências; Arquiteto: brief.
        let report_md = ReportGenerator::render(&evidence)?;
        let decision = ReportGenerator::decide(&evidence);
        let brief_md = match brief {
            Some(input) => Some(BriefGenerator::render(input)?),
            None => None,
        };

        // 4. Auditoria da rodada no Blackboard.
        self.blackboard.put_tuple(
            "commissioning",
            "last_run",
            serde_json::json!({
                "date": evidence.date,
                "system": evidence.system,
                "version": evidence.version,
                "decision": format!("{:?}", decision),
                "tests_passed": evidence.tests.passed,
                "tests_failed": evidence.tests.failed,
                "stub_high": stub_report.high_severity_count,
                "stub_low": stub_report.low_severity_count,
                "files_scanned": stub_report.files_scanned,
                "brief_generated": brief_md.is_some(),
            }),
        )?;

        Ok(CommissioningArtifacts {
            brief_md,
            report_md,
            stub_report,
            decision,
        })
    }

    /// Escreve os artefatos em disco (`COMMISSIONING_REPORT.generated.md`,
    /// `PROJECT_BRIEF.generated.md`). O sufixo `.generated` evita sobrescrever
    /// artefatos aprovados por humanos — promoção é decisão do operador.
    pub fn write_to(&self, artifacts: &CommissioningArtifacts, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .with_context(|| format!("criando diretório {}", dir.display()))?;
        fs::write(
            dir.join("COMMISSIONING_REPORT.generated.md"),
            &artifacts.report_md,
        )?;
        if let Some(ref brief) = artifacts.brief_md {
            fs::write(dir.join("PROJECT_BRIEF.generated.md"), brief)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brief_generator::SuccessMetric;
    use crate::report_generator::{FlowEvidence, TestSummary};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn evidence() -> EvidencePack {
        EvidencePack {
            system: "O Arreio".into(),
            version: "4.7".into(),
            date: "2026-06-11".into(),
            environment: "teste".into(),
            flows: vec![FlowEvidence {
                id: "1".into(),
                action: "check".into(),
                expected: "ok".into(),
                observed: "ok".into(),
                passed: true,
            }],
            tests: TestSummary {
                passed: 10,
                failed: 0,
                ignored: 0,
                suites: 1,
            },
            stubs: None,
            pending: vec![],
            restrictions: vec![],
        }
    }

    fn brief_input() -> BriefInput {
        BriefInput {
            pvc_id: "PVC-META".into(),
            title: "Self-Commissioning".into(),
            owner: "@maintainer".into(),
            date: "2026-06-11".into(),
            problem: "O sistema precisa se comissionar.".into(),
            in_scope: vec!["Gerar artefatos".into()],
            out_of_scope: vec![],
            metrics: vec![SuccessMetric {
                metric: "Artefatos".into(),
                target: "gerados sem erro".into(),
            }],
            dependencies: vec![],
            risks: vec![],
        }
    }

    #[test]
    fn rodada_completa_com_codigo_limpo_aprova() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.rs"), "fn f() -> u8 { 1 }\n").unwrap();

        let bb = temp_bb();
        let commissioner = SelfCommissioner::new(bb.clone());
        let artifacts = commissioner
            .commission(dir.path(), evidence(), Some(&brief_input()))
            .unwrap();

        assert_eq!(artifacts.decision, CommissioningDecision::Aprovado);
        assert!(artifacts.brief_md.is_some());
        assert!(artifacts.report_md.contains("✅ Aprovado"));
        assert_eq!(artifacts.stub_report.files_scanned, 1);

        // Auditoria publicada.
        let audit = bb.get_tuple("commissioning", "last_run").unwrap();
        assert_eq!(audit["decision"], "Aprovado");
        assert_eq!(audit["tests_passed"], 10);
        assert_eq!(audit["brief_generated"], true);
    }

    #[test]
    fn stub_de_alta_severidade_gera_restricao() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stub.rs"), "fn f() { todo!() }\n").unwrap();

        let commissioner = SelfCommissioner::new(temp_bb());
        let artifacts = commissioner
            .commission(dir.path(), evidence(), None)
            .unwrap();

        assert_eq!(
            artifacts.decision,
            CommissioningDecision::AprovadoComRestricoes
        );
        assert!(artifacts.report_md.contains("stub.rs"));
        assert!(artifacts.brief_md.is_none());
    }

    #[test]
    fn teste_falhando_reprova() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.rs"), "fn f() {}\n").unwrap();

        let mut ev = evidence();
        ev.tests.failed = 2;
        let commissioner = SelfCommissioner::new(temp_bb());
        let artifacts = commissioner.commission(dir.path(), ev, None).unwrap();
        assert_eq!(artifacts.decision, CommissioningDecision::Reprovado);
        assert!(artifacts.report_md.contains("❌ Reprovado"));
    }

    #[test]
    fn write_to_usa_sufixo_generated() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("clean.rs"), "fn f() {}\n").unwrap();
        let out = tempfile::tempdir().unwrap();

        let commissioner = SelfCommissioner::new(temp_bb());
        let artifacts = commissioner
            .commission(src.path(), evidence(), Some(&brief_input()))
            .unwrap();
        commissioner.write_to(&artifacts, out.path()).unwrap();

        assert!(out.path().join("COMMISSIONING_REPORT.generated.md").exists());
        assert!(out.path().join("PROJECT_BRIEF.generated.md").exists());
    }
}
