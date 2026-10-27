use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const MAX_SKILL_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_FILES: usize = 64;
const MAX_DEPTH: usize = 6;

#[derive(Clone, Debug)]
pub struct SkillPackage {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub files: Vec<SkillFile>,
}

#[derive(Clone, Debug)]
pub struct SkillFile {
    pub relative: PathBuf,
    pub bytes: Vec<u8>,
}

pub fn default_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("src/modules"),
        // 整体插件化：插件的技能包在 plugins/<模块>/skills/。
        PathBuf::from("plugins"),
        PathBuf::from("/etc/ru41/modules"),
    ]
}

pub fn load_roots(roots: &[PathBuf]) -> Result<Vec<SkillPackage>> {
    let mut packages = Vec::new();
    let mut names = HashSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        for module in read_dir(root)? {
            let skills = module.join("skills");
            if !skills.exists() {
                continue;
            }
            ensure_not_symlink(&skills)?;
            for skill in read_dir(&skills)? {
                let package = load_skill(&skill)?;
                if !names.insert(package.name.clone()) {
                    bail!("duplicate skill name: {}", package.name);
                }
                packages.push(package);
            }
        }
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

fn read_dir(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        ensure_not_symlink(&path)?;
        entries.push(path);
    }
    entries.sort();
    Ok(entries)
}

fn ensure_not_symlink(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("skill path must not be a symlink: {}", path.display());
    }
    Ok(())
}

fn load_skill(directory: &Path) -> Result<SkillPackage> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("skill directory has invalid name")?
        .to_owned();
    ensure!(valid_name(&name), "invalid skill name: {name}");
    let entrypoint = directory.join("SKILL.md");
    let metadata = fs::symlink_metadata(&entrypoint)?;
    ensure!(metadata.is_file(), "missing {}", entrypoint.display());
    ensure!(metadata.len() <= MAX_SKILL_BYTES, "SKILL.md is too large");
    let text = fs::read_to_string(&entrypoint)
        .with_context(|| format!("cannot read {}", entrypoint.display()))?;
    let (frontmatter, instructions) = split_frontmatter(&text)?;
    let mut parsed_name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" => parsed_name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    ensure!(
        parsed_name.as_deref() == Some(name.as_str()),
        "skill frontmatter name must match directory: {name}"
    );
    let description = description.filter(|value| !value.is_empty());
    let description = description.context("skill description is required")?;
    ensure!(
        !instructions.trim().is_empty(),
        "skill instructions are required"
    );

    let mut files = Vec::new();
    collect_files(directory, directory, 0, &mut files)?;
    Ok(SkillPackage {
        name,
        description,
        instructions,
        files,
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn split_frontmatter(text: &str) -> Result<(String, String)> {
    let normalized = text.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        bail!("skill must start with YAML frontmatter");
    }
    let end = normalized[4..]
        .find("\n---\n")
        .map(|index| index + 4)
        .context("unterminated skill frontmatter")?;
    Ok((
        normalized[4..end].to_owned(),
        normalized[end + 4..].trim().to_owned(),
    ))
}

fn unquote(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return value[1..value.len() - 1].to_owned();
    }
    value.to_owned()
}

fn collect_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<SkillFile>,
) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "skill nesting exceeds depth limit");
    for path in read_dir(directory)? {
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(root)
            .context("skill file escapes package")?
            .to_path_buf();
        if metadata.is_dir() {
            collect_files(root, &path, depth + 1, files)?;
        } else {
            ensure!(
                metadata.is_file(),
                "unsupported skill file: {}",
                path.display()
            );
            ensure!(
                metadata.len() <= MAX_SKILL_BYTES,
                "skill file is too large: {}",
                path.display()
            );
            let current_bytes: u64 = files
                .iter()
                .map(|file: &SkillFile| file.bytes.len() as u64)
                .sum();
            ensure!(
                files.len() < MAX_PACKAGE_FILES
                    && current_bytes + metadata.len() <= MAX_PACKAGE_BYTES,
                "skill package exceeds file count or size limit"
            );
            files.push(SkillFile {
                relative,
                bytes: fs::read(&path)?,
            });
        }
    }
    Ok(())
}

pub fn materialize(run_dir: &Path, packages: &[SkillPackage]) -> Result<()> {
    for package in packages {
        let base = run_dir.join(".agents/skills").join(&package.name);
        for file in &package.files {
            let destination = base.join(&file.relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(destination, &file.bytes)?;
        }
    }
    Ok(())
}

/// codex 专用技能索引前缀：codex 不把 .agents/skills 自动挂进上下文
/// （它原生只看 $CODEX_HOME/skills），所以在 prompt 开头列名称+简介+路径，
/// 模型按需自行 Read 对应 SKILL.md。只列索引不内联全文，控制 prompt 体积。
pub fn skills_index_prefix(skills: &[SkillPackage]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "可用技能（已在工作目录 .agents/skills/ 下，匹配需求时先读对应 SKILL.md 再行动）：\n",
    );
    for skill in skills {
        out.push_str(&format!(
            "- {}（.agents/skills/{}/SKILL.md）：{}\n",
            skill.name, skill.name, skill.description
        ));
    }
    out.push('\n');
    out
}

pub fn render_for_prompt(prompt: &str, packages: &[SkillPackage]) -> String {
    if packages.is_empty() {
        return prompt.to_owned();
    }
    let mut output = String::from(
        "The following Agent Skills are available. Follow a skill when it matches the request; otherwise ignore it.\n\n",
    );
    for package in packages {
        output.push_str(&format!(
            "## Skill: {}\n{}\n\n{}\n\n",
            package.name, package.description, package.instructions
        ));
    }
    output.push_str("\nUser request:\n");
    output.push_str(prompt);
    output
}
