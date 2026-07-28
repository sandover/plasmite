//! Purpose: Execute pool validation commands and select their presentation.
//! Exports: `DoctorArgs`, `run`.
//! Role: Keep diagnostic orchestration separate from storage validation.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use crate::{
    doctor_report, emit_doctor_human, emit_doctor_human_summary, list_pool_paths, report_json,
    resolve_poolref,
};
use plasmite::api::{Error, ErrorKind, LocalClient, PoolRef, ValidationStatus, to_exit_code};
use serde_json::json;

pub(super) struct DoctorArgs {
    pub(super) pool: Option<String>,
    pub(super) all: bool,
    pub(super) json: bool,
}

pub(super) fn run(args: DoctorArgs, context: &CliContext) -> Result<CommandResult, Error> {
    if args.all && args.pool.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--all cannot be combined with a pool name")
            .with_hint("Use --all by itself, or provide a single pool."));
    }
    if !args.all && args.pool.is_none() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("doctor requires a pool name or --all")
            .with_hint("Use `plasmite doctor <pool>` or `plasmite doctor --all`."));
    }

    let pool_dir = context.pool_dir();
    let client = LocalClient::new().with_pool_dir(pool_dir);
    let reports = if let Some(pool) = args.pool {
        let path = resolve_poolref(&pool, pool_dir)?;
        let pool_ref = PoolRef::path(path.clone());
        vec![doctor_report(&client, pool_ref, pool, path)?]
    } else {
        let mut reports = Vec::new();
        for path in list_pool_paths(pool_dir)? {
            let label = path.to_string_lossy().to_string();
            let pool_ref = PoolRef::path(path.clone());
            reports.push(doctor_report(&client, pool_ref, label, path)?);
        }
        reports
    };

    if args.json {
        let values = reports.iter().map(report_json).collect::<Vec<_>>();
        emit_json(json!({ "reports": values }), context.color_mode());
    } else if args.all {
        emit_doctor_human_summary(&reports);
    } else {
        for report in &reports {
            emit_doctor_human(report);
        }
    }

    let has_corrupt = reports
        .iter()
        .any(|report| report.status == ValidationStatus::Corrupt);
    let exit_code = if has_corrupt {
        to_exit_code(ErrorKind::Corrupt)
    } else {
        0
    };
    Ok(CommandResult::with_code(exit_code))
}
