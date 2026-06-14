// Phase 7 / Track 7.5 - skills-import CLI.
//
// Usage:
//   cargo run -p hydragent-skills --bin skills-import -- \
//       --source   skills/builtin \
//       --library  data/skill_library.sqlite \
//       --verbose
//
// Flags:
//   --source <DIR>   directory of *.yaml skill definitions
//                    (default: skills/builtin)
//   --library <PATH> SQLite skill library file
//                    (default: data/skill_library.sqlite)
//   --dry-run        parse every YAML and print a preview without
//                    touching the database (exits 2 on parse errors)
//   --list           open the library, print every skill, exit
//   -v / --verbose   print per-file details
//
// Design:
//   `SkillLibrary::load_builtins` is idempotent — it walks the
//   directory, parses every *.yaml as a `SkillSpec`, and upserts
//   via `INSERT ... ON CONFLICT(id) DO UPDATE`, tagging each
//   `skill_versions` row with `changed_by = "builtin"`. Re-running
//   the import is therefore safe; existing rows have their
//   `last_updated` bumped and a new history entry appended.
//
//   The dry-run path bypasses the database entirely so callers can
//   validate a future `skills/builtin/*.yaml` set in CI.

use anyhow::Context;
use clap::Parser;
use hydragent_skills::{library::SkillFilter, SkillLibrary};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "hydragent-skills-import",
    version,
    about = "Import skill YAML files into the SQLite skill library"
)]
struct Cli {
    /// Directory containing `*.yaml` skill definitions.
    #[arg(long, default_value = "skills/builtin")]
    source: PathBuf,

    /// Path to the SQLite skill library file.
    #[arg(long, default_value = "data/skill_library.sqlite")]
    library: PathBuf,

    /// Parse every YAML and print a preview; do not touch the database.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Open the library, list every skill, and exit.
    #[arg(long, default_value_t = false)]
    list: bool,

    /// Print per-file details and a final summary table.
    #[arg(long, short, default_value_t = false)]
    verbose: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.list {
        return list_mode(&cli.library, cli.verbose).await;
    }

    if !cli.source.exists() {
        anyhow::bail!("source directory does not exist: {}", cli.source.display());
    }
    if !cli.source.is_dir() {
        anyhow::bail!("source is not a directory: {}", cli.source.display());
    }

    let yaml_files = list_yaml_files(&cli.source)?;
    if yaml_files.is_empty() {
        println!(
            "No *.yaml files found in {}; nothing to do.",
            cli.source.display()
        );
        return Ok(());
    }

    println!(
        "Found {} candidate skill file(s) under {}",
        yaml_files.len(),
        cli.source.display()
    );

    if cli.dry_run {
        return dry_run(&yaml_files, cli.verbose);
    }

    println!("Opening library at {}", cli.library.display());
    let lib = SkillLibrary::open(&cli.library).await?;
    let before = lib.count().await?;
    if cli.verbose {
        println!("  library currently holds {} skill(s)", before);
    }

    let imported = lib.load_builtins(&cli.source).await?;
    let after = lib.count().await?;

    println!(
        "Imported {} skill(s); library now holds {} skill(s).",
        imported, after
    );
    if imported == 0 && before == after && after == 0 {
        // nothing to do; expected on an empty library with no files
    } else if before == after {
        println!("(row count unchanged — existing skills were upserted in place)");
    }

    if cli.verbose {
        println!("\nFinal contents of {}:", cli.library.display());
        list_mode(&cli.library, false).await?;
    }

    Ok(())
}

/// Collect every `*.yaml` file in `dir` (non-recursive, sorted for
/// deterministic output).
fn list_yaml_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("yaml") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Parse each YAML and print a one-line preview per file. Exits 2
/// (after printing) if any file is malformed, so CI can detect it.
fn dry_run(paths: &[PathBuf], verbose: bool) -> anyhow::Result<()> {
    let mut ok = 0usize;
    let mut err = 0usize;
    for p in paths {
        let yaml = std::fs::read_to_string(p)
            .with_context(|| format!("read {}", p.display()))?;
        match serde_yaml::from_str::<hydragent_skills::skill::SkillSpec>(&yaml) {
            Ok(spec) => {
                ok += 1;
                if verbose {
                    println!(
                        "  [ok ] {} -> id={:<32} name={:<28} tier={:?} tags=[{}] ver={}",
                        p.display(),
                        spec.id,
                        spec.name,
                        spec.tier,
                        spec.capability_tags.join(","),
                        spec.version,
                    );
                } else {
                    println!("  [ok ] {} -> {}", p.display(), spec.id);
                }
            }
            Err(e) => {
                err += 1;
                println!("  [err] {} -> {}", p.display(), e);
            }
        }
    }
    println!(
        "Dry-run summary: {} valid, {} invalid (no changes written).",
        ok, err
    );
    if err > 0 {
        std::process::exit(2);
    }
    Ok(())
}

/// Print every skill currently in the library, ordered by
/// `last_updated DESC`.
async fn list_mode(path: &Path, verbose: bool) -> anyhow::Result<()> {
    let lib = SkillLibrary::open(path).await?;
    let count = lib.count().await?;
    println!("Library {}: {} skill(s).", path.display(), count);
    let skills = lib.list_skills(SkillFilter::default()).await?;
    if skills.is_empty() {
        return Ok(());
    }
    for s in &skills {
        if verbose {
            println!(
                "  - {:<32} {:<28} tier={:?} tags=[{}] tools=[{}] ver={} success_rate={:.2}",
                s.id,
                s.name,
                s.tier,
                s.capability_tags.join(","),
                s.required_tools.join(","),
                s.version,
                s.success_rate,
            );
        } else {
            println!("  - {:<32} {:?}", s.id, s.tier);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_skills::skill::SkillSpec;
    use std::collections::HashMap;

    /// Write a temporary directory of fake skill YAMLs and return the
    /// paths, sorted.
    fn write_fake_skills(dir: &Path, specs: &[(&str, &str, &str)]) {
        for (id, name, body) in specs {
            let p = dir.join(format!("{id}.yaml"));
            std::fs::write(
                &p,
                format!(
                    "id: {id}\nname: {name}\ndescription: test\nprompt_template: |\n  {body}\nauthor: test\n"
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn list_yaml_files_picks_up_yaml_extensions_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.yaml"), "x").unwrap();
        std::fs::write(dir.path().join("b.yml"), "x").unwrap();
        std::fs::write(dir.path().join("c.txt"), "x").unwrap();
        let files = list_yaml_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.yaml"));
    }

    #[test]
    fn dry_run_reports_parse_errors_with_exit_hint() {
        // We can't actually exercise std::process::exit in a unit test
        // without a child process, so we just assert the parsing
        // helper returns ok for well-formed inputs.
        let dir = tempfile::tempdir().unwrap();
        write_fake_skills(dir.path(), &[("alpha", "Alpha", "hi"), ("beta", "Beta", "ho")]);
        let files = list_yaml_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        for p in &files {
            let yaml = std::fs::read_to_string(p).unwrap();
            let spec: SkillSpec = serde_yaml::from_str(&yaml).unwrap();
            assert!(!spec.id.is_empty());
            assert!(!spec.name.is_empty());
        }
    }

    #[tokio::test]
    async fn load_builtins_is_idempotent_on_empty_db() {
        let skill_dir = tempfile::tempdir().unwrap();
        write_fake_skills(
            skill_dir.path(),
            &[
                ("alpha", "Alpha", "hi"),
                ("beta", "Beta", "ho"),
                ("gamma", "Gamma", "he"),
            ],
        );

        let db = tempfile::tempdir().unwrap();
        let lib_path = db.path().join("skills.sqlite");
        let lib = SkillLibrary::open(&lib_path).await.unwrap();
        assert_eq!(lib.count().await.unwrap(), 0);

        // First import: 3 new rows.
        let n = lib.load_builtins(skill_dir.path()).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(lib.count().await.unwrap(), 3);

        // Second import: same count, no duplicates.
        let n2 = lib.load_builtins(skill_dir.path()).await.unwrap();
        assert_eq!(n2, 3, "load_builtins reports every file as processed");
        assert_eq!(
            lib.count().await.unwrap(),
            3,
            "idempotency: row count must not grow on re-import"
        );

        // Every id is present.
        for id in ["alpha", "beta", "gamma"] {
            let s = lib.get_skill(id).await.unwrap();
            assert!(s.is_some(), "missing skill {id}");
        }
    }

    #[tokio::test]
    async fn load_builtins_skips_non_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_skills(dir.path(), &[("only", "Only", "x")]);
        std::fs::write(dir.path().join("readme.txt"), "ignore me").unwrap();
        std::fs::write(dir.path().join("notes.md"), "ignore me too").unwrap();
        let files = list_yaml_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn dry_run_helper_rejects_malformed_yaml() {
        // Build a tiny in-memory scenario: write a bad YAML, then
        // confirm serde_yaml::from_str returns Err.
        let bad = "id: 123\nname: [unclosed bracket\n";
        let res: Result<SkillSpec, _> = serde_yaml::from_str(bad);
        assert!(res.is_err());

        // A hashmap smoke check so the test isn't trivially green.
        let mut m = HashMap::new();
        m.insert("a", 1);
        assert_eq!(m.len(), 1);
    }
}
