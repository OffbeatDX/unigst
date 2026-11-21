use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::common::tags::Format;

#[derive(Parser, Debug)]
#[command(
    name = "unigst",
    version = crate::VERSION,
    about = "Unified GST generator for arcade rhythm games"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Sdvx(SdvxArgs),

    Iidx(IidxArgs),

    Popn(PopnArgs),
}

#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
    #[arg(short, long)]
    pub input: PathBuf,

    #[arg(short, long, default_value = "GST")]
    pub output: PathBuf,

    #[arg(short, long, default_value = "opus")]
    pub format: Format,

    #[arg(short, long, default_value_t = 0)]
    pub jobs: usize,

    #[arg(long, default_value_t = false)]
    pub overwrite: bool,

    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    #[arg(long, default_value_t = false)]
    pub json: bool,

    #[arg(long, default_value_t = false)]
    pub verbose: bool,
}

impl CommonArgs {
    pub fn thread_count(&self) -> usize {
        if self.jobs == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        } else {
            self.jobs
        }
    }
}

#[derive(Args, Debug)]
pub struct SdvxArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    #[arg(short = 'v', long)]
    pub version: Option<String>,

    #[arg(long, num_args = 1..)]
    pub id: Vec<String>,
}

#[derive(Args, Debug)]
pub struct IidxArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    #[arg(short = 'v', long)]
    pub version: Option<u16>,

    #[arg(long, num_args = 1..)]
    pub id: Vec<u32>,
}

#[derive(Args, Debug)]
pub struct PopnArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    #[arg(short = 'v', long)]
    pub version: Option<u32>,

    #[arg(long, num_args = 1..)]
    pub id: Vec<u32>,

    #[arg(long)]
    pub dll: Option<PathBuf>,
}
