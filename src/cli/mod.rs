//! Purpose: Own CLI execution boundaries below argument parsing.
//! Exports: `CliContext`, `CommandResult`, and `dispatch`.
//! Role: Route parsed commands to cohesive command-family modules.
//! Invariants: Command modules depend on explicit context and output helpers.

mod access;
pub(crate) mod args;
mod context;
mod doctor;
pub(crate) mod doctor_support;
mod feed;
pub(crate) mod feed_support;
pub(super) mod output;
pub(crate) mod output_support;
mod pool;
pub(crate) mod pool_support;
mod result;
mod server;
mod stream;
pub(crate) mod stream_support;
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
        Command::Serve { run } => server::run(run, &context),
        Command::Access { command } => access::run(command, &context),
        Command::Mcp { dir, remote } => utility::run(
            utility::UtilityCommand::Mcp {
                pool_dir: dir,
                remote,
            },
            &context,
        ),
        Command::Completion { shell } => {
            utility::run(utility::UtilityCommand::Completion { shell }, &context)
        }
    }
}
