//! CLI helpers for doctor commands.

use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::LocalClient;
use plasmite::api::PoolRef;
use plasmite::api::ValidationIssue;
use plasmite::api::ValidationReport;
use plasmite::api::ValidationStatus;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::IsTerminal;
use std::path::PathBuf;

use super::output_support::format_seq_range;
use super::output_support::short_display_path;
use super::pool_support::message_count_from_info;

pub(crate) fn emit_doctor_human(report: &ValidationReport) {
    if !io::stdout().is_terminal() {
        let label = report
            .pool_ref
            .clone()
            .unwrap_or_else(|| report.path.to_string_lossy().to_string());
        let label = super::output_support::human_literal(&label);
        match report.status {
            ValidationStatus::Ok => {
                println!("OK: {label}");
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
                println!("CORRUPT: {label}{last_good}{issue}");
            }
        }
        return;
    }

    let label = doctor_display_label(report);
    match report.status {
        ValidationStatus::Ok => {
            println!("{label}: healthy");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!("  checked:   header, index, ring — 0 issues");
        }
        ValidationStatus::Corrupt => {
            let issue = report
                .issues
                .first()
                .map(|value| value.message.clone())
                .unwrap_or_else(|| "corruption detected".to_string());
            println!("{label}: corrupt");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!(
                "  checked:   header, index, ring — {} issues",
                report.issues.len()
            );
            println!(
                "  detail:    {}",
                super::output_support::human_literal(&issue)
            );
        }
    }
}

pub(crate) fn emit_doctor_human_summary(reports: &[ValidationReport]) {
    if reports.is_empty() {
        println!("No pools found.");
        return;
    }
    if !io::stdout().is_terminal() {
        for report in reports {
            emit_doctor_human(report);
        }
        return;
    }

    let corrupt = reports
        .iter()
        .filter(|report| report.status == ValidationStatus::Corrupt)
        .count();
    let labels = reports.iter().map(doctor_display_label).collect::<Vec<_>>();
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
        println!("All {} pools healthy.", reports.len());
        println!();
        for idx in 0..reports.len() {
            println!(
                "  {:<label_width$}   {:<message_width$}   0 issues",
                labels[idx], message_labels[idx]
            );
        }
    } else {
        println!("{corrupt} of {} pools unhealthy.", reports.len());
        println!();
        for (idx, report) in reports.iter().enumerate() {
            let label = &labels[idx];
            let messages = &message_labels[idx];
            if report.status == ValidationStatus::Corrupt {
                println!(
                    "  ✗ {:<label_width$}   {:<message_width$}   {} issues (run `pls doctor {}` for detail)",
                    label,
                    messages,
                    report.issues.len(),
                    label
                );
            } else {
                println!("  ✓ {label:<label_width$}   {messages:<message_width$}   0 issues");
            }
        }
    }
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

pub(crate) fn doctor_messages_summary(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        let seq_range = format_seq_range(stats.oldest_seq, stats.newest_seq);
        if stats.count == 0 {
            return "empty".to_string();
        }
        if seq_range == "-" {
            return stats.count.to_string();
        }
        return format!("{} ({seq_range})", stats.count);
    }

    let seq_range = format_seq_range(report.last_good_seq, report.last_good_seq);
    if seq_range == "-" {
        "empty".to_string()
    } else {
        format!("visible count unavailable ({seq_range})")
    }
}

pub(crate) fn doctor_messages_count_label(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        return format!("{} messages", stats.count);
    }
    if report.status == ValidationStatus::Ok {
        "messages unknown".to_string()
    } else {
        report
            .last_good_seq
            .map(|seq| format!("up to seq {seq}"))
            .unwrap_or_else(|| "messages unknown".to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DoctorMessageStats {
    count: u64,
    oldest_seq: Option<u64>,
    newest_seq: Option<u64>,
}

pub(crate) fn doctor_message_stats(report: &ValidationReport) -> Option<DoctorMessageStats> {
    let info = LocalClient::new()
        .pool_info(&PoolRef::path(report.path.clone()))
        .ok()?;
    Some(DoctorMessageStats {
        count: message_count_from_info(&info),
        oldest_seq: info.bounds.oldest_seq,
        newest_seq: info.bounds.newest_seq,
    })
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
) -> Result<ValidationReport, Error> {
    match client.validate_pool(&pool_ref) {
        Ok(report) => Ok(report.with_pool_ref(label)),
        Err(err) if err.kind() == ErrorKind::Corrupt => {
            Ok(ValidationReport::corrupt(path, error_issue(&err), None).with_pool_ref(label))
        }
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
