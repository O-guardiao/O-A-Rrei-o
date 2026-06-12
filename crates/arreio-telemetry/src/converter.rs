//! Conversor de estruturas O Arreio para spans OTLP.
//!
//! Cada `TrajectoryEntry` vira um `OtelSpan` com atributos e events
//! derivados dos campos do entry.

use crate::otel::*;
use arreio_kernel::{TrajectoryEntry, TrajectoryResult};

/// Converte um `TrajectoryEntry` em `OtelSpan`.
///
/// O trace_id é derivado do hash do task_id (determinístico para
/// correlacionar retries da mesma tarefa). O span_id é aleatório.
pub fn trajectory_to_otel(entry: &TrajectoryEntry) -> OtelSpan {
    let trace_id = derive_trace_id(&entry.task_id);
    let span_id = gen_span_id();
    let start_nano = secs_to_nanos(entry.timestamp);
    let end_nano = start_nano + millis_to_nanos(entry.duration_ms);

    let mut attributes = vec![
        OtelAttribute::string("task_id", &entry.task_id),
        OtelAttribute::int("attempt", entry.attempt_number as i64),
        OtelAttribute::int("tokens_consumed", entry.tokens_consumed as i64),
        OtelAttribute::string("models_used", &entry.models_used.join(",")),
    ];

    if let Some(cmd) = &entry.validation_cmd {
        attributes.push(OtelAttribute::string("validation_cmd", cmd));
    }

    if let Some(hash) = &entry.code_hash {
        attributes.push(OtelAttribute::string("code_hash", hash));
    }

    // Status do span baseado no resultado da trajetória
    let (status, status_msg) = match &entry.result {
        TrajectoryResult::Success { test_count, test_passed } => {
            attributes.push(OtelAttribute::int("test_count", *test_count as i64));
            attributes.push(OtelAttribute::int("test_passed", *test_passed as i64));
            (StatusCode::Ok, None)
        }
        TrajectoryResult::Failure { exit_code, error_summary } => {
            attributes.push(OtelAttribute::int("exit_code", *exit_code as i64));
            (
                StatusCode::Error,
                Some(format!("exit_code={} error={}", exit_code, error_summary)),
            )
        }
        TrajectoryResult::Timeout { duration_ms } => {
            attributes.push(OtelAttribute::int("timeout_ms", *duration_ms as i64));
            (StatusCode::Error, Some("timeout".into()))
        }
        TrajectoryResult::Blocked { reason } => {
            (StatusCode::Error, Some(format!("blocked: {}", reason)))
        }
    };

    // HITL status como atributo
    attributes.push(OtelAttribute::string(
        "hitl_status",
        format!("{:?}", entry.hitl_status),
    ));

    // Violations de contrato como atributo
    if !entry.contract_violations.is_empty() {
        attributes.push(OtelAttribute::int(
            "contract_violations",
            entry.contract_violations.len() as i64,
        ));
        for (i, v) in entry.contract_violations.iter().enumerate() {
            attributes.push(OtelAttribute::string(
                format!("contract_violation.{}", i),
                format!("{:?}: {}", v.violation_type, v.details),
            ));
        }
    }

    // Eventos: decisão humana (HITL)
    let mut events = Vec::new();
    if let Some(decision) = &entry.human_decision {
        let mut ev = OtelEvent::new("hitl.decision", secs_to_nanos(decision.timestamp));
        ev.attributes.push(OtelAttribute::string(
            "decision",
            format!("{:?}", decision.decision),
        ));
        ev.attributes.push(OtelAttribute::string(
            "approver",
            &decision.approver_identity,
        ));
        ev.attributes.push(OtelAttribute::string(
            "context_hash",
            &decision.context_hash,
        ));
        if let Some(policy) = &decision.policy_name {
            ev.attributes.push(OtelAttribute::string("policy", policy));
        }
        if let Some(just) = &decision.justification {
            ev.attributes.push(OtelAttribute::string("justification", just));
        }
        events.push(ev);
    }

    // Eventos: violações de contrato
    for v in &entry.contract_violations {
        events.push(
            OtelEvent::new("contract.violation", v.timestamp_ms * 1_000_000)
                .with_attribute(OtelAttribute::string("type", format!("{:?}", v.violation_type)))
                .with_attribute(OtelAttribute::string("contract_id", &v.contract_id))
                .with_attribute(OtelAttribute::string("details", &v.details)),
        );
    }

    OtelSpan {
        trace_id,
        span_id,
        parent_span_id: None,
        name: format!("arreio.task.{}", entry.task_id),
        kind: Some(SpanKind::Internal),
        start_time_unix_nano: start_nano,
        end_time_unix_nano: end_nano,
        attributes,
        events,
        status: Some(OtelStatus {
            code: status,
            message: status_msg,
        }),
        resource: Some(OtelResource::new()),
    }
}

/// Deriva um trace_id determinístico a partir do task_id.
/// Isso garante que retries da mesma tarefa compartilhem o mesmo trace.
pub fn derive_trace_id(task_id: &str) -> TraceId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(task_id.as_bytes());
    let result = hasher.finalize();
    // Pegamos os primeiros 16 bytes (128 bits) do hash
    let bytes: [u8; 16] = result[..16].try_into().unwrap_or_default();
    format!("{:032x}", u128::from_be_bytes(bytes))
}

/// Converte múltiplos entries em spans.
pub fn trajectories_to_otel(entries: &[TrajectoryEntry]) -> Vec<OtelSpan> {
    entries.iter().map(trajectory_to_otel).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::{ApprovalDecision, ContractViolation, HitlStatus, HumanDecision, TrajectoryResult, ViolationType};

    fn make_entry(task_id: &str, success: bool) -> TrajectoryEntry {
        TrajectoryEntry {
            task_id: task_id.into(),
            timestamp: 1_717_000_000,
            specification: "test spec".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: Some("abc123".into()),
            validation_cmd: Some("cargo test".into()),
            result: if success {
                TrajectoryResult::Success {
                    test_count: 5,
                    test_passed: 5,
                }
            } else {
                TrajectoryResult::Failure {
                    exit_code: 1,
                    error_summary: "compile error".into(),
                }
            },
            models_used: vec!["deepseek-v4-pro".into()],
            tokens_consumed: 1500,
            duration_ms: 3200,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: HitlStatus::NotApplicable,
            human_decision: None,
        }
    }

    #[test]
    fn success_entry_becomes_ok_span() {
        let entry = make_entry("task_001", true);
        let span = trajectory_to_otel(&entry);

        assert_eq!(span.name, "arreio.task.task_001");
        assert_eq!(span.status.as_ref().unwrap().code, StatusCode::Ok);
        assert!(span.attributes.iter().any(|a| a.key == "task_id"));
        assert!(span.attributes.iter().any(|a| a.key == "test_count"));
    }

    #[test]
    fn failure_entry_becomes_error_span() {
        let entry = make_entry("task_002", false);
        let span = trajectory_to_otel(&entry);

        assert_eq!(span.status.as_ref().unwrap().code, StatusCode::Error);
        assert!(span
            .status
            .as_ref()
            .unwrap()
            .message
            .as_ref()
            .unwrap()
            .contains("exit_code=1"));
    }

    #[test]
    fn trace_id_is_deterministic_for_same_task() {
        let entry1 = make_entry("task_A", true);
        let entry2 = make_entry("task_A", true);
        let span1 = trajectory_to_otel(&entry1);
        let span2 = trajectory_to_otel(&entry2);

        assert_eq!(span1.trace_id, span2.trace_id, "trace_id deve ser determinístico");
        assert_ne!(span1.span_id, span2.span_id, "span_id deve ser diferente");
    }

    #[test]
    fn hitl_decision_becomes_event() {
        let mut entry = make_entry("task_003", true);
        entry.hitl_status = HitlStatus::Approved;
        entry.human_decision = Some(HumanDecision {
            task_id: "task_003".into(),
            decision: ApprovalDecision::Approved,
            approver_identity: "admin".into(),
            approver_roles: vec!["admin".into()],
            context_hash: "hash123".into(),
            timestamp: 1_717_000_001,
            justification: Some("looks good".into()),
            policy_name: Some("financial_tx".into()),
            escalation_level: 0,
        });

        let span = trajectory_to_otel(&entry);
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].name, "hitl.decision");
        assert!(span
            .events[0]
            .attributes
            .iter()
            .any(|a| a.key == "approver"));
    }

    #[test]
    fn contract_violations_as_events_and_attributes() {
        let mut entry = make_entry("task_004", false);
        entry.contract_violations = vec![ContractViolation {
            contract_id: "c1".into(),
            violation_type: ViolationType::SchemaMismatch,
            node_id: "node_1".into(),
            details: "field missing".into(),
            timestamp_ms: 1_717_000_000_000,
        }];

        let span = trajectory_to_otel(&entry);
        assert!(span.attributes.iter().any(|a| a.key == "contract_violations"));
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].name, "contract.violation");
    }

    #[test]
    fn batch_conversion() {
        let entries = vec![
            make_entry("task_001", true),
            make_entry("task_002", false),
        ];
        let spans = trajectories_to_otel(&entries);
        assert_eq!(spans.len(), 2);
    }
}
