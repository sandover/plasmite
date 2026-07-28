//! Purpose: Own CLI execution boundaries below argument parsing.
//! Exports: `CliContext`, `CommandResult`, and `dispatch`.
//! Role: Route parsed commands to cohesive command-family modules.
//! Invariants: Command modules depend on explicit context and output helpers.

pub(crate) mod args;
mod context;
mod doctor;
mod feed;
pub(super) mod output;
mod pool;
mod result;
mod server;
mod stream;
pub(crate) mod support;
mod tap;
mod utility;

pub(super) use context::CliContext;
pub(super) use result::CommandResult;

use args::Command;
use plasmite::api::Error;

pub(super) fn dispatch(command: Command, context: CliContext) -> Result<CommandResult, Error> {
    match command {
        Command::Version => utility::run(utility::UtilityCommand::Version, &context),
        Command::Doctor { pool, all, json } => {
            doctor::run(doctor::DoctorArgs { pool, all, json }, &context)
        }
        Command::Pool { command } => pool::run(command, &context),
        Command::Feed {
            pool,
            tag,
            data,
            file,
            durability,
            create,
            create_size,
            retry,
            retry_delay,
            input,
            errors,
            token,
            token_file,
            tls_ca,
            tls_skip_verify,
        } => feed::run(
            feed::FeedArgs {
                pool,
                tags: tag,
                data,
                file,
                durability,
                create,
                create_size,
                retry,
                retry_delay,
                input,
                errors,
                token,
                token_file,
                tls_ca,
                tls_skip_verify,
            },
            &context,
        ),
        Command::Fetch { pool, seq } => feed::fetch(&pool, seq, &context),
        Command::Follow {
            pool,
            create,
            tail,
            one,
            jsonl,
            timeout,
            data_only,
            format,
            since,
            where_expr,
            tags,
            quiet_drops,
            no_notify,
            replay,
            token,
            token_file,
            tls_ca,
            tls_skip_verify,
        } => stream::follow(
            stream::FollowArgs {
                pool,
                create,
                tail,
                one,
                jsonl,
                timeout,
                data_only,
                format,
                since,
                where_expr,
                tags,
                quiet_drops,
                no_notify,
                replay,
                token,
                token_file,
                tls_ca,
                tls_skip_verify,
            },
            &context,
        ),
        Command::Duplex {
            pool,
            me,
            create,
            tail,
            jsonl,
            timeout,
            format,
            since,
            echo_self,
        } => stream::duplex(
            stream::DuplexArgs {
                pool,
                me,
                create,
                tail,
                jsonl,
                timeout,
                format,
                since,
                echo_self,
            },
            &context,
        ),
        Command::Tap {
            pool,
            create,
            create_size,
            tag,
            quiet,
            durability,
            command,
        } => tap::run(
            tap::TapArgs {
                pool,
                create,
                create_size,
                tags: tag,
                quiet,
                durability,
                command,
            },
            &context,
        ),
        Command::Serve { subcommand, run } => server::run(subcommand, run, &context),
        Command::Mcp { dir } => {
            utility::run(utility::UtilityCommand::Mcp { pool_dir: dir }, &context)
        }
        Command::Completion { shell } => {
            utility::run(utility::UtilityCommand::Completion { shell }, &context)
        }
    }
}
