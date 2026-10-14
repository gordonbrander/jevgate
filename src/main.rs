mod provider;

use clap::Parser;
use provider::{Judge, Provider};
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::time::Duration;

/// Run a command only if a Jev-style classifier says "yes" about stdin.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Yes/no question about the input.
    question: String,
    /// Pass if P(yes) >= this.
    #[arg(long, default_value_t = 0.5, value_parser = probability)]
    threshold: f64,
    /// Criteria for "yes".
    #[arg(long, requires = "no")]
    yes: Option<String>,
    /// Criteria for "no".
    #[arg(long, requires = "yes")]
    no: Option<String>,
    #[arg(long, value_enum, default_value_t = Provider::SystemOne)]
    provider: Provider,
    /// Model id (default depends on provider).
    #[arg(long)]
    model: Option<String>,
    /// Full endpoint URL (overrides the provider default).
    #[arg(long)]
    url: Option<String>,
    /// Per-request HTTP timeout in seconds.
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    /// Log every decision to stderr.
    #[arg(short, long)]
    verbose: bool,
    /// Command to run on pass (after `--`). Without one, exit 0/1 like `test`.
    #[arg(last = true)]
    command: Vec<String>,
}

fn probability(s: &str) -> Result<f64, String> {
    let p: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if (0.0..=1.0).contains(&p) {
        Ok(p)
    } else {
        Err("must be between 0 and 1".into())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(&cli).unwrap_or_else(|e| {
        eprintln!("jevgate: {e}");
        ExitCode::from(2)
    })
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("reading stdin: {e}"))?;
    if input.trim().is_empty() {
        return Err("empty input on stdin".into());
    }

    let judge = Judge {
        state: &input,
        question: &cli.question,
        criteria: cli.yes.as_deref().zip(cli.no.as_deref()),
    };
    let (score, model) = cli.provider.judge(
        &judge,
        cli.url.as_deref(),
        cli.model.as_deref(),
        Duration::from_secs(cli.timeout),
    )?;

    let pass = score >= cli.threshold;
    if cli.verbose {
        let verdict = if pass { "pass" } else { "reject" };
        eprintln!(
            "jevgate: {verdict} score={score:.2} threshold={:.2} model={model}",
            cli.threshold
        );
    }
    if !pass {
        return Ok(ExitCode::from(1));
    }
    let Some((prog, args)) = cli.command.split_first() else {
        return Ok(ExitCode::SUCCESS);
    };

    let mut child = Command::new(prog)
        .args(args)
        .env("JEVGATE_SCORE", score.to_string())
        .env("JEVGATE_MODEL", &model)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawning {prog}: {e}"))?;
    // Ignore errors: a child that doesn't read stdin closes the pipe early.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let status = child.wait().map_err(|e| format!("waiting for {prog}: {e}"))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(code as u8);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return ExitCode::from((128 + sig) as u8);
        }
    }
    ExitCode::from(2)
}
