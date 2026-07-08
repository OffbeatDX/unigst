use std::time::Instant;

use indicatif::{HumanDuration, ProgressBar, ProgressDrawTarget, ProgressStyle};

use super::report::{ProgressSink, RunReport};

const FIXED_COLS: usize = 60;

const AMBIGUOUS_MARGIN: usize = 12;

pub struct Reporter {
    bar: ProgressBar,
    verbose: bool,
    total: u64,
    started: Instant,

    msg_budget: usize,
}

impl Reporter {
    pub fn new(total: u64, verbose: bool) -> Self {
        let bar = if verbose {
            ProgressBar::hidden()
        } else {
            let b =
                ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr_with_hz(8));

            b.set_style(
                ProgressStyle::with_template("{spinner} [{bar:30}] {pos}/{len} ({eta}) {wide_msg}")
                    .unwrap()
                    .progress_chars("=> "),
            );

            b.enable_steady_tick(std::time::Duration::from_millis(125));
            b
        };

        let cols = console::Term::stderr().size().1 as usize;
        let msg_budget = cols.saturating_sub(FIXED_COLS + AMBIGUOUS_MARGIN).max(10);
        Reporter {
            bar,
            verbose,
            total,
            started: Instant::now(),
            msg_budget,
        }
    }

    pub fn finish(&self, report: &RunReport) {
        self.bar.finish_and_clear();

        if !self.verbose {
            if !report.warnings.is_empty() {
                eprintln!("{} warning(s):", report.warnings.len());
                for w in &report.warnings {
                    eprintln!("  {w}");
                }
            }
            if !report.failures.is_empty() {
                eprintln!("{} failure(s):", report.failures.len());
                for f in &report.failures {
                    eprintln!("  {f}");
                }
            }
        }
        let mut summary = format!("done: {}/{} encoded", report.encoded, self.total);
        if report.skipped > 0 {
            summary.push_str(&format!(", {} skipped (already existed)", report.skipped));
        }
        if !report.failures.is_empty() {
            summary.push_str(&format!(", {} FAILED", report.failures.len()));
        }
        if report.cancelled {
            summary.push_str(", cancelled");
        }
        summary.push_str(&format!(" — {}", HumanDuration(self.started.elapsed())));
        println!("{summary}");
    }
}

impl ProgressSink for Reporter {
    fn started(&self, title: &str) {
        let msg = console::truncate_str(title, self.msg_budget, "…").into_owned();
        self.bar.set_message(msg);
    }

    fn advanced(&self) {
        self.bar.inc(1);
    }

    fn warned(&self, msg: &str) {
        if self.verbose {
            eprintln!("warning: {msg}");
        }
    }

    fn logged(&self, msg: &str) {
        if self.verbose {
            println!("{msg}");
        }
    }
}
