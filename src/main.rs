//! `unigst`: the command line. `unigstgui` is the GUI build.

use clap::Parser;

use unigst::cli::Cli;

fn main() {
    let code = unigst::run::cli(Cli::parse());
    if code != 0 {
        std::process::exit(code);
    }
}
