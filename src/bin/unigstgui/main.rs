//! `unigstgui`: the GUI build. Command-line use is `unigst`'s job; this exe is
//! a Windows GUI program, so it gets no console (and no console flash).

#![cfg_attr(windows, windows_subsystem = "windows")]

mod gui;

fn main() {
    // No console: ffmpeg children would otherwise each pop a console window.
    unigst::common::ffmpeg::set_no_console_window(true);
    if let Err(err) = gui::run() {
        // No console to print to on Windows, so show the error in a dialog.
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("unigstgui")
            .set_description(format!(
                "Failed to start the GUI: {err}\n\nThe command-line version, unigst, does not need a GUI."
            ))
            .show();
        std::process::exit(1);
    }
}
