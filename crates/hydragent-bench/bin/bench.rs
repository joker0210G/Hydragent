// Phase 7 / Track 7.4 - Benchmarking harness CLI.
//
// Usage:
//   cargo run -p hydragent-bench --bin bench -- \
//       --skill-bench tests/bench/skill_bench_v1.jsonl \
//       --golden-set  tests/bench/golden_set_v1.jsonl \
//       --output      reports/bench-v0.7.0.json
//
// The retriever is currently a no-op stub that returns an empty
// list — wire it to hydragent_skills::SkillLibrary in Week 30 once
// the executor lands.

use clap::Parser;
use hydragent_bench::{
    dataset::{load_golden_set, load_skill_bench},
    report::BenchReport,
    runner::{GoldenScores, Retriever, SkillBenchScores},
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hydragent-bench", version, about = "SKILL-BENCH + golden set runner")]
struct Cli {
    /// Path to skill_bench_v1.jsonl (80 tasks)
    #[arg(long)]
    skill_bench: PathBuf,

    /// Path to golden_set_v1.jsonl (30 hand-verified pairs)
    #[arg(long)]
    golden_set: PathBuf,

    /// Optional output JSON path
    #[arg(long)]
    output: Option<PathBuf>,

    /// Report version string (e.g. "v0.7.0")
    #[arg(long, default_value = "v0.7.0")]
    report_version: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    println!("Loading skill_bench from {}", cli.skill_bench.display());
    let tasks = load_skill_bench(&cli.skill_bench)?;
    println!("  loaded {} tasks", tasks.len());

    println!("Loading golden_set from {}", cli.golden_set.display());
    let golden = load_golden_set(&cli.golden_set)?;
    println!("  loaded {} pairs", golden.len());

    // Stub retriever: returns empty list. Week 30 will wire this to
    // `hydragent_skills::SkillLibrary` for a real FTS5 / tag search.
    let retriever: Retriever = Box::new(|_q| Vec::<String>::new());

    let sb_scores = SkillBenchScores::compute(&tasks, &retriever);
    let gd_scores = GoldenScores::compute(&golden, &retriever);

    let report = BenchReport::new(cli.report_version)
        .with_skill_bench(sb_scores)
        .with_golden_set(gd_scores);
    report.print_summary();

    if let Some(out) = cli.output {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(&out, json)?;
        println!("\nWrote report to {}", out.display());
    }

    Ok(())
}
