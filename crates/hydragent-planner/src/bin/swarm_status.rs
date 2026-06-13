//! # `swarm_status` — dump a DAG or an execution report as ASCII
//!
//! Phase 5 / Track 5.4. Tiny CLI front-end for the ASCII printer in
//! `hydragent_planner::ascii`. Reads a JSON file from disk, deserialises
//! it, and prints a human-readable picture on stdout.
//!
//! ## Usage
//!
//! ```text
//! # Render a saved DagSpec (typical: data/swarm/<id>/dag.json)
//! swarm_status --from-spec ./data/swarm/s-42/dag.json
//!
//! # Render a saved ExecutionReport (post-run)
//! swarm_status --from-report ./data/swarm/s-42/report.json
//!
//! # Pipe a JSON spec on stdin
//! cat dag.json | swarm_status --stdin-spec
//! ```
//!
//! Exit code: `0` on success, `2` on IO/parse error.
//!
//! This is a diagnostic / operator tool, not part of the agent's
//! hot path. Keep it dependency-light — `clap` is the only non-workspace
//! dep, and it's already in the workspace root.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use hydragent_planner::ascii::{print_dag, print_report, print_run_outcome};
use hydragent_planner::dag::DagSpec;
use hydragent_planner::dag_execution::{ExecutionReport, RunOutcome};

#[derive(Parser, Debug)]
#[command(
    name = "swarm_status",
    version,
    about = "Render a saved DagSpec or ExecutionReport as ASCII"
)]
struct Cli {
    /// Path to a JSON-encoded `DagSpec`. Mutually exclusive with
    /// `--from-report` and `--stdin-spec`.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["from_report", "stdin_spec", "stdin_report"])]
    from_spec: Option<PathBuf>,

    /// Path to a JSON-encoded `ExecutionReport`.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["from_spec", "stdin_spec", "stdin_report"])]
    from_report: Option<PathBuf>,

    /// Read a `DagSpec` JSON document from stdin.
    #[arg(long, conflicts_with_all = ["from_spec", "from_report", "stdin_report"])]
    stdin_spec: bool,

    /// Read a `RunOutcome` JSON document from stdin. `RunOutcome` is
    /// serialised as `{ "success": true, "report": { ... } }` or
    /// `{ "success": false, "report": { ... }, "error": "..." }`.
    #[arg(long, conflicts_with_all = ["from_spec", "from_report", "stdin_spec"])]
    stdin_report: bool,

    /// Suppress the header block (swarm_id / page_id / task / totals).
    /// Useful for diffing two runs.
    #[arg(long)]
    no_header: bool,

    /// Print a one-line summary suitable for log shipping.
    #[arg(long)]
    one_line: bool,
}

impl Cli {
    /// Exactly one input source must be selected.
    fn input_kind(&self) -> InputKind {
        if self.from_spec.is_some() {
            InputKind::SpecFile
        } else if self.from_report.is_some() {
            InputKind::ReportFile
        } else if self.stdin_spec {
            InputKind::StdinSpec
        } else if self.stdin_report {
            InputKind::StdinReport
        } else {
            InputKind::None
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum InputKind {
    SpecFile,
    ReportFile,
    StdinSpec,
    StdinReport,
    None,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin = read_stdin_if_needed(&cli);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    match render(&cli, &stdin, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(out, "swarm_status: error: {e}");
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render<W: Write>(cli: &Cli, stdin_payload: &str, out: &mut W) -> anyhow::Result<()> {
    let kind = cli.input_kind();
    match kind {
        InputKind::None => {
            anyhow::bail!(
                "no input source specified. Use one of: --from-spec, --from-report, \
                 --stdin-spec, --stdin-report. Try --help for details."
            );
        }
        InputKind::SpecFile => {
            let path = cli.from_spec.as_deref().expect("checked by input_kind");
            let bytes = std::fs::read(path)?;
            let spec: DagSpec = serde_json::from_slice(&bytes)?;
            emit_dag(&spec, cli, out)
        }
        InputKind::ReportFile => {
            let path = cli.from_report.as_deref().expect("checked by input_kind");
            let bytes = std::fs::read(path)?;
            let report: ExecutionReport = serde_json::from_slice(&bytes)?;
            emit_report(&report, cli, out)
        }
        InputKind::StdinSpec => {
            let spec: DagSpec = serde_json::from_str(stdin_payload)?;
            emit_dag(&spec, cli, out)
        }
        InputKind::StdinReport => {
            let parsed: StdinRunOutcome = serde_json::from_str(stdin_payload)?;
            emit_stdin_run_outcome(&parsed, cli, out)
        }
    }
}

fn emit_dag<W: Write>(spec: &DagSpec, cli: &Cli, out: &mut W) -> anyhow::Result<()> {
    if cli.one_line {
        writeln!(out, "{}", one_line_spec(spec))?;
        return Ok(());
    }
    let picture = print_dag(spec);
    if cli.no_header {
        // Strip the first 5 lines (the header).
        for line in picture.lines().skip(5) {
            writeln!(out, "{line}")?;
        }
    } else {
        out.write_all(picture.as_bytes())?;
    }
    Ok(())
}

fn emit_report<W: Write>(report: &ExecutionReport, cli: &Cli, out: &mut W) -> anyhow::Result<()> {
    if cli.one_line {
        writeln!(out, "{}", one_line_report(report))?;
        return Ok(());
    }
    let picture = print_report(report);
    if cli.no_header {
        for line in picture.lines().skip(5) {
            writeln!(out, "{line}")?;
        }
    } else {
        out.write_all(picture.as_bytes())?;
    }
    Ok(())
}

fn emit_stdin_run_outcome<W: Write>(
    parsed: &StdinRunOutcome,
    cli: &Cli,
    out: &mut W,
) -> anyhow::Result<()> {
    let outcome: RunOutcome = if parsed.success {
        RunOutcome::Success(parsed.report.clone())
    } else {
        RunOutcome::Failed(
            parsed.report.clone(),
            hydragent_planner::dag_execution::EngineError::NodeFailed(
                parsed.error.clone().unwrap_or_else(|| "unknown".into()),
            ),
        )
    };
    if cli.one_line {
        let r = outcome.report();
        let status = if outcome.is_success() { "OK" } else { "FAIL" };
        writeln!(
            out,
            "swarm={} page={} status={} completed={} failed={} skipped={} wall_ms={}",
            r.swarm_id,
            r.page_id,
            status,
            r.completed,
            r.failed,
            r.skipped,
            r.total_execution_ms
        )?;
        return Ok(());
    }
    let picture = print_run_outcome(&outcome);
    if cli.no_header {
        for line in picture.lines().skip(5) {
            writeln!(out, "{line}")?;
        }
    } else {
        out.write_all(picture.as_bytes())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct StdinRunOutcome {
    success: bool,
    report: ExecutionReport,
    error: Option<String>,
}

fn read_stdin_if_needed(cli: &Cli) -> String {
    match cli.input_kind() {
        InputKind::StdinSpec | InputKind::StdinReport => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
        _ => String::new(),
    }
}

fn one_line_spec(spec: &DagSpec) -> String {
    let mut counts = [0usize; 6];
    for n in &spec.nodes {
        counts[status_index(&n.status)] += 1;
    }
    format!(
        "swarm={} page={} nodes={} edges={} pending={} ready={} running={} completed={} failed={} skipped={}",
        spec.swarm_id,
        spec.page_id,
        spec.nodes.len(),
        spec.edges.len(),
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
    )
}

fn one_line_report(report: &ExecutionReport) -> String {
    format!(
        "swarm={} page={} completed={} failed={} cancelled={} skipped={} wall_ms={} status={}",
        report.swarm_id,
        report.page_id,
        report.completed,
        report.failed,
        report.cancelled,
        report.skipped,
        report.total_execution_ms,
        if report.is_success() { "OK" } else { "FAIL" }
    )
}

fn status_index(s: &hydragent_planner::dag::NodeStatus) -> usize {
    use hydragent_planner::dag::NodeStatus::*;
    match s {
        Pending => 0,
        Ready => 1,
        Running => 2,
        Completed => 3,
        Failed => 4,
        Skipped => 5,
    }
}

#[allow(dead_code)]
fn path_label(p: &Path) -> &str {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("<unknown>")
}
