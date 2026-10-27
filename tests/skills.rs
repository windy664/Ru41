use ru41::skills::{load_roots, materialize, render_for_prompt};
use std::path::Path;

fn write_skill(root: &Path, module: &str, skill: &str, name: &str, description: &str) {
    let directory = root.join(module).join("skills").join(skill);
    std::fs::create_dir_all(directory.join("references")).unwrap();
    std::fs::write(
        directory.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {description}\n---\n\nFollow the test workflow.\n"
        ),
    )
    .unwrap();
    std::fs::write(directory.join("references/rules.md"), "test reference").unwrap();
}

#[test]
fn loads_and_materializes_native_skills() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "papers",
        "arxiv-research",
        "arxiv-research",
        "Research papers",
    );
    let packages = load_roots(&[root.path().to_path_buf()]).unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "arxiv-research");
    assert_eq!(packages[0].description, "Research papers");
    assert_eq!(packages[0].files.len(), 2);

    let run = root.path().join("run");
    materialize(&run, &packages).unwrap();
    assert!(run.join(".agents/skills/arxiv-research/SKILL.md").is_file());
    assert!(
        run.join(".agents/skills/arxiv-research/references/rules.md")
            .is_file()
    );
    let prompt = render_for_prompt("Explain this paper", &packages);
    assert!(prompt.contains("Follow the test workflow"));
    assert!(prompt.ends_with("User request:\nExplain this paper"));
}

#[test]
fn rejects_duplicate_names_and_bad_frontmatter() {
    let root = tempfile::tempdir().unwrap();
    write_skill(root.path(), "papers", "shared", "shared", "one");
    write_skill(root.path(), "writing", "shared", "shared", "two");
    let error = load_roots(&[root.path().to_path_buf()]).unwrap_err();
    assert!(error.to_string().contains("duplicate skill name"));

    write_skill(
        root.path(),
        "bad",
        "missing-description",
        "missing-description",
        "",
    );
    let error = load_roots(&[root.path().to_path_buf()]).unwrap_err();
    assert!(error.to_string().contains("description is required"));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_skill_directories() {
    let root = tempfile::tempdir().unwrap();
    write_skill(root.path(), "papers", "linked", "linked", "linked");
    let target = root.path().join("papers/skills/linked");
    let link = root.path().join("papers/skills/shortcut");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let error = load_roots(&[root.path().to_path_buf()]).unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));
}

#[test]
fn bundled_module_skills_are_valid() {
    let packages = ru41::skills::load_roots(&ru41::skills::default_roots()).unwrap();
    assert!(packages.iter().any(|package| {
        package.name == "arxiv-research" && package.instructions.contains("arXiv ID")
    }));
    assert!(packages.iter().any(|package| {
        package.name == "ecard-utility"
            && package.instructions.contains("/水电")
            && package.instructions.contains("不要重新查询")
    }));
}
