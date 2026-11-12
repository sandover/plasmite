//! CLI helpers for doctor commands.

use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::LocalClient;
use plasmite::api::PoolRef;
use plasmite::api::ValidationIssue;
use plasmite::api::ValidationObservation;
use plasmite::api::ValidationReport;
use plasmite::api::ValidationStatus;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::IsTerminal;
use std::path::PathBuf;

use super::output_support::format_seq_range;
use super::output_support::short_display_path;

pub(crate) fn emit_doctor_human(observation: &ValidationObservation) -> Result<(), Error> {
    let report = &observation.report;
    if !io::stdout().is_terminal() {
        let label = report
            .pool_ref
            .clone()
            .unwrap_or_else(|| report.path.to_string_lossy().to_string());
        let label = super::output_support::human_literal(&label);
        match report.status {
            ValidationStatus::Ok => {
                super::output::write_stdout(format!("OK: {label}\n").as_bytes())?;
            }
            ValidationStatus::Corrupt => {
                let last_good = report
                    .last_good_seq
                    .map(|seq| format!(" last_good_seq={seq}"))
                    .unwrap_or_default();
                let issue = report
                    .issues
                    .first()
                    .map(|issue| {
                        format!(
                            " issue={}",
                            super::output_support::human_literal(&issue.message)
                        )
                    })
                    .unwrap_or_default();
                super::output::write_stdout(
                    format!("CORRUPT: {label}{last_good}{issue}\n").as_bytes(),
                )?;
            }
        }
        return Ok(());
    }

    let label = doctor_display_label(report);
    match report.status {
        ValidationStatus::Ok => {
            super::output::write_stdout(format!("{label}: healthy\n").as_bytes())?;
            super::output::write_stdout(
                format!("  messages:  {}\n", doctor_messages_summary(observation)).as_bytes(),
            )?;
            super::output::write_stdout(
                "  checked:   header, index, ring — 0 issues\n".as_bytes(),
            )?;
        }
        ValidationStatus::Corrupt => {
            let issue = report
                .issues
                .first()
                .map(|value| value.message.clone())
                .unwrap_or_else(|| "corruption detected".to_string());
            super::output::write_stdout(format!("{label}: corrupt\n").as_bytes())?;
            super::output::write_stdout(
                format!("  messages:  {}\n", doctor_messages_summary(observation)).as_bytes(),
            )?;
            super::output::write_stdout(
                format!(
                    "  checked:   header, index, ring — {} issues\n",
                    report.issues.len()
                )
                .as_bytes(),
            )?;
            super::output::write_stdout(
                format!(
                    "  detail:    {}\n",
                    super::output_support::human_literal(&issue)
                )
                .as_bytes(),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn emit_doctor_human_summary(reports: &[ValidationObservation]) -> Result<(), Error> {
    if reports.is_empty() {
        super::output::write_stdout(b"No pools found.\n")?;
        return Ok(());
    }
    if !io::stdout().is_terminal() {
        for report in reports {
            emit_doctor_human(report)?;
        }
        return Ok(());
    }

    let corrupt = reports
        .iter()
        .filter(|observation| observation.report.status == ValidationStatus::Corrupt)
        .count();
    let labels = reports
        .iter()
        .map(|observation| doctor_display_label(&observation.report))
        .collect::<Vec<_>>();
    let message_labels = reports
        .iter()
        .map(doctor_messages_count_label)
        .collect::<Vec<_>>();
    let label_width = labels.iter().map(|value| value.len()).max().unwrap_or(0);
    let message_width = message_labels
        .iter()
        .map(|value| value.len())
        .max()
        .unwrap_or(0);
    if corrupt == 0 {
        super::output::write_stdout(format!("All {} pools healthy.\n", reports.len()).as_bytes())?;
        super::output::write_stdout(b"\n")?;
        for idx in 0..reports.len() {
            super::output::write_stdout(
                format!(
                    "  {:<label_width$}   {:<message_width$}   0 issues\n",
                    labels[idx], message_labels[idx]
                )
                .as_bytes(),
            )?;
        }
    } else {
        super::output::write_stdout(
            format!("{corrupt} of {} pools unhealthy.\n", reports.len()).as_bytes(),
        )?;
        super::output::write_stdout(b"\n")?;
        for (idx, observation) in reports.iter().enumerate() {
            let report = &observation.report;
            let label = &labels[idx];
            let messages = &message_labels[idx];
            if report.status == ValidationStatus::Corrupt {
                super::output::write_stdout(format!(
                    "  ✗ {:<label_width$}   {:<message_width$}   {} issues (run `pls doctor {}` for detail)\n",
                    label,
                    messages,
                    report.issues.len(),
                    label
                ).as_bytes())?;
            } else {
                super::output::write_stdout(
                    format!("  ✓ {label:<label_width$}   {messages:<message_width$}   0 issues\n")
                        .as_bytes(),
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn doctor_display_label(report: &ValidationReport) -> String {
    if let Some(pool_ref) = report.pool_ref.as_deref() {
        let looks_like_path = pool_ref.contains('/') || pool_ref.contains('\\');
        if !looks_like_path {
            return super::output_support::human_literal(pool_ref);
        }
    }
    if let Some(stem) = report.path.file_stem().and_then(|value| value.to_str()) {
        return super::output_support::human_literal(stem);
    }
    super::output_support::human_literal(&short_display_path(&report.path, report.path.parent()))
}

pub(crate) fn doctor_messages_summary(observation: &ValidationObservation) -> String {
    if let Some(stats) = observation.messages {
        let seq_range = format_seq_range(stats.oldest_seq, stats.newest_seq);
        if stats.count == 0 {
            return "empty".to_string();
        }
        if seq_range == "-" {
            return stats.count.to_string();
        }
        return format!("{} ({seq_range})", stats.count);
    }

    "messages unknown".to_string()
}

pub(crate) fn doctor_messages_count_label(observation: &ValidationObservation) -> String {
    observation
        .messages
        .map(|stats| format!("{} messages", stats.count))
        .unwrap_or_else(|| "messages unknown".to_string())
}

pub(crate) fn report_json(report: &ValidationReport) -> Value {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            json!({
                "code": issue.code,
                "message": issue.message,
                "seq": issue.seq,
                "offset": issue.offset,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "pool_ref": report.pool_ref,
        "path": report.path.to_string_lossy(),
        "status": match report.status {
            ValidationStatus::Ok => "ok",
            ValidationStatus::Corrupt => "corrupt",
        },
        "last_good_seq": report.last_good_seq,
        "issue_count": report.issue_count,
        "issues": issues,
        "remediation_hints": report.remediation_hints,
        "snapshot_path": report.snapshot_path.as_ref().map(|path| path.to_string_lossy()),
    })
}

pub(crate) fn doctor_report(
    client: &LocalClient,
    pool_ref: PoolRef,
    label: String,
    path: PathBuf,
) -> Result<ValidationObservation, Error> {
    match client.validate_pool_observation(&pool_ref) {
        Ok(mut observation) => {
            observation.report = observation.report.with_pool_ref(label);
            Ok(observation)
        }
        Err(err) if err.kind() == ErrorKind::Corrupt => Ok(ValidationObservation {
            report: ValidationReport::corrupt(path, error_issue(&err), None).with_pool_ref(label),
            messages: None,
        }),
        Err(err) => Err(err),
    }
}

pub(crate) fn error_issue(err: &Error) -> ValidationIssue {
    ValidationIssue {
        code: "corrupt".to_string(),
        message: err.message().unwrap_or("corrupt").to_string(),
        seq: err.seq(),
        offset: err.offset(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasmite::api::{Durability, PoolApiExt, PoolOptions};

    #[test]
    fn diagnostic_facts_survive_append_replacement_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let reference = PoolRef::name("observed");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        let mut pool = client.open_pool(&reference).unwrap();
        for _ in 0..2 {
            pool.append_json_now(&json!({"x": 1}), &[], Durability::Fast)
                .unwrap();
        }
        let observation = client.validate_pool_observation(&reference).unwrap();
        let json_before = report_json(&observation.report);
        pool.append_json_now(&json!({"x": 2}), &[], Durability::Fast)
            .unwrap();
        assert_eq!(doctor_messages_summary(&observation), "2 (seq 1..2)");
        drop(pool);
        client.delete_pool(&reference).unwrap();
        assert_eq!(doctor_messages_count_label(&observation), "2 messages");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        assert_eq!(doctor_messages_summary(&observation), "2 (seq 1..2)");
        assert_eq!(report_json(&observation.report), json_before);
        client.delete_pool(&reference).unwrap();
        std::fs::write(dir.path().join("observed.plasmite"), b"invalid").unwrap();
        let corrupt = doctor_report(
            &client,
            reference,
            "observed".into(),
            dir.path().join("observed.plasmite"),
        )
        .unwrap();
        assert_eq!(doctor_messages_summary(&corrupt), "messages unknown");
    }
}
