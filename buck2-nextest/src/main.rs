// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use buck2_nextest::App;
use clap::Parser;
use nextest_session::WriteStr;
use std::io::Write;

fn main() -> std::process::ExitCode {
    let cli_args: Vec<String> = std::env::args().collect();
    let app = App::parse();

    // Standard output carries the JSON Buck2's callbacks parse, so nothing else
    // may be written to it; errors go to standard error, which Buck2 keeps
    // alongside the action's result.
    let mut stdout = std::io::stdout();
    let mut writer = StdoutWriter(&mut stdout);

    match app.exec(&mut writer, cli_args) {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(error) => {
            let code = error.exit_code();
            eprintln!("{:?}", miette::Report::new(error));
            std::process::ExitCode::from(code as u8)
        }
    }
}

struct StdoutWriter<'a>(&'a mut std::io::Stdout);

impl WriteStr for StdoutWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        self.0.write_all(s.as_bytes())
    }

    fn write_str_flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
