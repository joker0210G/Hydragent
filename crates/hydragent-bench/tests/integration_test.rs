//! End-to-end integration test: load real SKILL-BENCH + golden set
//! data, populate a real `SkillLibrary`, run the runner with a
//! `SkillLibrary`-backed retriever, and assert sensible baselines.

use hydragent_bench::{
    dataset::{load_golden_set, load_skill_bench},
    report::BenchReport,
    runner::{GoldenScores, Retriever, SkillBenchScores},
};
use hydragent_skills::{library::SkillFilter, skill::skill_from_yaml, SkillLibrary, SkillTier};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::tempdir;

fn workspace_path(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/hydragent-bench -> crates
    p.pop(); // crates -> repo root
    p.push(rel);
    p
}

fn skills_dir() -> PathBuf { workspace_path("skills/builtin") }

fn rt_block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(f)
}

async fn open_library_with_builtins(db_path: &Path) -> SkillLibrary {
    let lib = SkillLibrary::open(db_path).await.expect("open library");
    let dir = skills_dir();
    if dir.is_dir() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).unwrap();
            if let Ok(skill) = skill_from_yaml(&text) {
                let _ = lib.insert_skill(&skill).await;
            }
        }
    }
    lib
}

fn make_retriever(lib: Arc<SkillLibrary>) -> Retriever {
    Box::new(move |query: &str| {
        let lib = lib.clone();
        let q = query.to_string();
        let results = rt_block_on(async move { lib.search_by_keyword(&q, 5).await });
        match results {
            Ok(v) => v.into_iter().map(|s| s.id).collect(),
            Err(_) => Vec::new(),
        }
    })
}

#[test]
fn loads_skill_bench_data_file() {
    let path = workspace_path("tests/bench/skill_bench_v1.jsonl");
    let tasks = load_skill_bench(&path).expect("load");
    assert!(tasks.len() >= 80, "expected 80+ tasks, got {}", tasks.len());
    assert!(tasks.iter().all(|t| !t.query.is_empty()));
    assert!(tasks.iter().all(|t| !t.expected_skill.is_empty()));
}

#[test]
fn loads_golden_set_data_file() {
    let path = workspace_path("tests/bench/golden_set_v1.jsonl");
    let items = load_golden_set(&path).expect("load");
    assert!(items.len() >= 30, "expected 30+ pairs, got {}", items.len());
    for it in &items {
        assert!(!it.query.is_empty());
        assert!(!it.relevant.is_empty(), "item {} has empty relevant", it.id);
    }
}

#[test]
fn library_with_builtins_runs_skill_bench_runner() {
    let dir = tempdir().unwrap();
    let lib = rt_block_on(open_library_with_builtins(&dir.path().join("skills.db")));
    let builtin_count = rt_block_on(async {
        lib.list_skills(SkillFilter {
            tier: Some(SkillTier::Active),
            ..SkillFilter::default()
        }).await.unwrap().len()
    });
    assert!(builtin_count > 0, "expected at least one active skill to be loaded");

    let tasks = load_skill_bench(&workspace_path("tests/bench/skill_bench_v1.jsonl"))
        .expect("load skill bench");
    let lib_arc = Arc::new(lib);
    let r = make_retriever(lib_arc);
    let sb = SkillBenchScores::compute(&tasks, &r);
    assert_eq!(sb.n, tasks.len());
    // baseline smoke: scores parse and are in [0, 1]
    assert!((0.0..=1.0).contains(&sb.recall_at_1));
    assert!((0.0..=1.0).contains(&sb.recall_at_3));
    assert!((0.0..=1.0).contains(&sb.recall_at_5));
    assert!((0.0..=1.0).contains(&sb.mrr));
}

#[test]
fn library_with_builtins_runs_golden_runner() {
    let dir = tempdir().unwrap();
    let lib = rt_block_on(open_library_with_builtins(&dir.path().join("skills.db")));
    let items = load_golden_set(&workspace_path("tests/bench/golden_set_v1.jsonl"))
        .expect("load golden");
    let lib_arc = Arc::new(lib);
    let r = make_retriever(lib_arc);
    let gd = GoldenScores::compute(&items, &r);
    assert_eq!(gd.n, items.len());
    assert!((0.0..=1.0).contains(&gd.mean_precision));
    assert!((0.0..=1.0).contains(&gd.mean_recall));
    assert!((0.0..=1.0).contains(&gd.mean_f1));
}

#[test]
fn report_serializes_with_real_scores() {
    let dir = tempdir().unwrap();
    let lib = rt_block_on(open_library_with_builtins(&dir.path().join("skills.db")));
    let lib_arc = Arc::new(lib);
    let r = make_retriever(lib_arc);

    let tasks = load_skill_bench(&workspace_path("tests/bench/skill_bench_v1.jsonl"))
        .expect("load skill bench");
    let items = load_golden_set(&workspace_path("tests/bench/golden_set_v1.jsonl"))
        .expect("load golden");

    let report = BenchReport::new("v0.7.0")
        .with_skill_bench(SkillBenchScores::compute(&tasks, &r))
        .with_golden_set(GoldenScores::compute(&items, &r));

    let json = serde_json::to_string_pretty(&report).unwrap();
    assert!(json.contains("v0.7.0"));
    assert!(json.contains("recall_at_1"));
    assert!(json.contains("mean_f1"));
}
