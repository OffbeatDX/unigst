//! The command-line entry point (`unigst`).

use std::sync::Arc;

use anyhow::Result;

use crate::cli::{Cli, Command, CommonArgs};
use crate::common::progress::Reporter;
use crate::common::report::{self, CancelToken, Plan, ProgressSink, RunReport};
use crate::{iidx, popn, sdvx};

/// Run a parsed command line. Returns the process exit code.
pub fn cli(cli: Cli) -> i32 {
    let result = match cli.command {
        Command::Sdvx(args) => run(sdvx::plan(&args), &args.common, |p, sink, c| {
            sdvx::execute(&args, p, sink, c)
        }),
        Command::Iidx(args) => run(iidx::plan(&args), &args.common, |p, sink, c| {
            iidx::execute(&args, p, sink, c)
        }),
        Command::Popn(args) => run(popn::plan(&args), &args.common, |p, sink, c| {
            popn::execute(&args, p, sink, c)
        }),
    };

    match result {
        Ok(failures) if failures > 0 => 1,
        Ok(_) => 0,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    }
}

fn run<P, F>(plan: Result<P>, common: &CommonArgs, execute: F) -> Result<usize>
where
    P: Plan,
    F: FnOnce(&P, &dyn ProgressSink, &CancelToken) -> Result<RunReport>,
{
    let plan = plan?;

    if common.dry_run {
        if common.json {
            println!("{}", report::tracks_to_json(&plan.tracks()));
        } else {
            if common.verbose {
                for line in plan.detail_lines() {
                    println!("{line}");
                }
            } else {
                for t in plan.tracks() {
                    println!("{}", t.summary_line());
                }
            }
            for w in plan.warnings() {
                eprintln!("warning: {w}");
            }
            println!("{}", plan.dry_run_summary());
        }
        return Ok(0);
    }

    if plan.is_empty() {
        for w in plan.warnings() {
            eprintln!("warning: {w}");
        }
        println!("no songs matched the selection — nothing to do");
        return Ok(0);
    }

    let cancel = install_ctrlc();
    let reporter = Reporter::new(plan.len() as u64, common.verbose);
    let report = execute(&plan, &reporter, &cancel)?;
    reporter.finish(&report);
    Ok(report.failures.len())
}

fn install_ctrlc() -> Arc<CancelToken> {
    let cancel = Arc::new(CancelToken::new());
    let token = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if token.is_cancelled() {
            std::process::exit(130);
        }
        token.cancel();
        eprintln!("\ncancelling — waiting for in-flight tracks (Ctrl-C again to abort)");
    });
    cancel
}
