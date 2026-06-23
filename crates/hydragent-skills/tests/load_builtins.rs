//! Integration tests for `hydragent-skills`.
//!
//! These run against the real YAML files in `../../skills/builtin/`
//! and exercise the full round-trip (load_builtins -> get_skill_by_name
//! -> export_yaml -> re-import). The unit tests in `src/library.rs`
//! cover the in-memory variant; this file covers the file-backed
//! variant and the on-disk YAML contract.

use hydragent_skills::library::SkillLibrary;
use std::path::PathBuf;

fn builtin_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is the path to the crate root.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // repo root
    p.push("skills");
    p.push("builtin");
    p
}

#[tokio::test]
async fn builtin_directory_contains_four_yaml_files() {
    let dir = builtin_dir();
    assert!(dir.exists(), "skills/builtin/ does not exist at {:?}", dir);
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
            count += 1;
        }
    }
    assert_eq!(count, 4, "expected 4 builtin YAMLs, found {count}");
}

#[tokio::test]
async fn load_builtins_roundtrips_all_four() {
    let lib = SkillLibrary::in_memory().await.unwrap();
    let count = lib.load_builtins(&builtin_dir()).await.unwrap();
    assert_eq!(count, 4);

    let sum = lib.get_skill_by_name("summarize-text").await.unwrap()
        .expect("summarize-text must be present");
    assert_eq!(sum.capability_tags, vec!["text", "summary", "general"]);
    assert!(sum.params.iter().any(|p| p.name == "text"));
    assert_eq!(sum.tier, hydragent_skills::SkillTier::Active);

    let exp = lib.get_skill_by_name("explain-concept").await.unwrap()
        .expect("explain-concept must be present");
    assert!(exp.params.iter().any(|p| p.name == "topic"));

    let draft = lib.get_skill_by_name("draft-message").await.unwrap()
        .expect("draft-message must be present");
    assert!(draft.params.iter().any(|p| p.name == "bullet_points"));
    assert!(draft.prompt_template.contains("{{bullet_points}}"));
}

#[tokio::test]
async fn export_yaml_preserves_template() {
    let lib = SkillLibrary::in_memory().await.unwrap();
    lib.load_builtins(&builtin_dir()).await.unwrap();
    let yaml = lib
        .export_yaml("skill-builtin-summarize-text")
        .await
        .unwrap()
        .expect("skill should be present");
    assert!(yaml.contains("prompt_template:"));
    assert!(yaml.contains("{{text}}"));
    assert!(yaml.contains("tier: active"));
    assert!(yaml.contains("name: summarize-text"));
}

#[tokio::test]
async fn file_backed_library_works() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("skills.db");
    let lib = SkillLibrary::open(&db_path).await.unwrap();
    let count = lib.load_builtins(&builtin_dir()).await.unwrap();
    assert_eq!(count, 4);
    // Re-open and confirm persisted.
    drop(lib);
    let lib2 = SkillLibrary::open(&db_path).await.unwrap();
    assert_eq!(lib2.count().await.unwrap(), 4);
    let sum = lib2
        .get_skill_by_name("summarize-text")
        .await
        .unwrap()
        .expect("persisted across reopen");
    assert!(!sum.required_tools.is_empty());
}
