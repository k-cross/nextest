// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use buck2_nextest::App;
use clap::Parser;
use nextest_session::WriteStr;
use std::io::Write;
use tracing_subscriber::{
    Layer,
    filter::{LevelFilter, Targets},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Sends nextest's warnings to stderr.
///
/// Without a subscriber every `tracing` warning nextest raises is discarded,
/// so an ignored configuration key or an undetectable rustc libdir would be
/// dropped silently. Buck2 reads this process's stdout for the result JSON,
/// so the logs go to stderr, which Buck2 shows with the test's own output.
fn init_logging() {
    let targets = match std::env::var("NEXTEST_LOG") {
        Ok(filter) if !filter.is_empty() => match filter.parse::<Targets>() {
            Ok(targets) => targets,
            Err(error) => {
                eprintln!("ignoring NEXTEST_LOG, which could not be parsed: {error}");
                Targets::new().with_default(LevelFilter::INFO)
            }
        },
        _ => Targets::new().with_default(LevelFilter::INFO),
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .without_time()
                .with_target(false)
                .with_filter(targets),
        )
        .init();
}

fn main() -> std::process::ExitCode {
    init_logging();
    let cli_args: Vec<String> = std::env::args().collect();
    let app = App::parse();

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
