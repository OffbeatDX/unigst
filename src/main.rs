use std::sync::Arc;

use anyhow::Result;
use clap::Parser;

use unigst::cli::{Cli, Command, CommonArgs};
use unigst::common::progress::Reporter;
use unigst::common::report::{self, CancelToken, Plan, ProgressSink, RunReport};
use unigst::{iidx, popn, sdvx};

fn main() {
    let cli = Cli::parse();
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
        Ok(failures) if failures > 0 => std::process::exit(1),
        Ok(_) => {}
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(1);
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
            for line in plan.detail_lines() {
                println!("{line}");
            }
            for w in plan.warnings() {
                eprintln!("warning: {w}");
            }
            println!("{}", plan.dry_run_summary());
        }
        return Ok(0);
    }

    if plan.nothing_to_do() {
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
