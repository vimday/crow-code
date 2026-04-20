use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum output length before truncation (yomi pattern: 40K chars).
pub const MAX_OUTPUT_LENGTH: usize = 40_000;
const TRUNCATION_MESSAGE: &str = "\n\n[Output truncated due to length.]";

/// Truncate output if it exceeds max length (UTF-8 safe).
pub fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_LENGTH {
        return output.to_string();
    }
    // Find a safe UTF-8 boundary near the limit
    let mut end = MAX_OUTPUT_LENGTH;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MESSAGE}", &output[..end])
}

/// A loaded skill with metadata (no body content — injected on-demand per Yomi pattern)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    #[serde(skip)]
    pub source_path: PathBuf,
}

/// Plugin descriptor for loading skills from external plugins.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub path: PathBuf,
    pub skills_path: Option<PathBuf>,
    pub skills_paths: Vec<PathBuf>,
}

/// Frontmatter metadata for a skill
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    description: String,
    #[serde(default)]
    triggers: Vec<String>,
}

/// Skill loader that scans directories for SKILL.md files
#[derive(Debug, Clone)]
pub struct SkillLoader {
    folders: Vec<PathBuf>,
}

impl SkillLoader {
    pub fn new(folders: Vec<PathBuf>) -> Self {
        Self { folders }
    }

    /// Load all skills from configured folders
    pub fn load_all(&self) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();

        for folder in &self.folders {
            if folder.exists() {
                Self::load_from_folder(folder, folder, &mut skills)?;
            }
        }

        let mut seen_names = std::collections::HashSet::new();
        skills.retain(|skill| {
            if seen_names.contains(&skill.name) {
                false
            } else {
                seen_names.insert(skill.name.clone());
                true
            }
        });
        Ok(skills)
    }

    fn load_from_folder(root: &Path, current: &Path, skills: &mut Vec<Skill>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                Self::load_from_folder(root, &path, skills)?;
            } else if path.is_file() {
                let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if file_name.ends_with("SKILL.md") || file_name.ends_with("skill.md") {
                    if let Ok(skill) = Self::load_skill(&path, root) {
                        skills.push(skill);
                    }
                }
            }
        }
        Ok(())
    }

    /// Load a single skill from a file.
    /// Only reads the YAML frontmatter portion for efficiency (Yomi pattern).
    /// The body content is NOT loaded — the LLM can request it on-demand.
    fn load_skill(path: &Path, root_folder: &Path) -> Result<Skill> {
        use std::io::{BufRead, BufReader};

        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open skill file: {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // Check if file starts with ---
        let first_line = lines.next().transpose()?;
        if first_line.as_deref() != Some("---") {
            anyhow::bail!("Skill file must start with frontmatter delimiter ---");
        }

        // Collect frontmatter lines until second ---
        let mut yaml_lines = Vec::new();
        let mut found_end = false;
        for line in lines {
            let line = line?;
            if line == "---" {
                found_end = true;
                break;
            }
            yaml_lines.push(line);
        }

        if !found_end {
            anyhow::bail!("Frontmatter end delimiter not found");
        }

        let yaml_content = yaml_lines.join("\n");
        let frontmatter: SkillFrontmatter = serde_yaml::from_str(&yaml_content)
            .context("Failed to parse skill frontmatter YAML")?;

        let skill_name = Self::derive_skill_name(path, root_folder);

        Ok(Skill {
            name: skill_name,
            description: frontmatter.description,
            triggers: frontmatter.triggers,
            source_path: path.to_path_buf(),
        })
    }

    fn derive_skill_name(path: &Path, root_folder: &Path) -> String {
        let relative = path.strip_prefix(root_folder).unwrap_or(path);
        let components: Vec<_> = relative
            .parent()
            .into_iter()
            .flat_map(|p| p.components())
            .filter_map(|c| {
                if let std::path::Component::Normal(os_str) = c {
                    os_str.to_str()
                } else {
                    None
                }
            })
            .collect();

        if components.is_empty() {
            relative
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unnamed")
                .to_string()
        } else {
            components.join(":")
        }
    }

    // ── Async operations (yomi pattern) ──────────────────────────────────────

    /// Find a skill file by name in configured folders (async).
    pub async fn find_skill_file(&self, name: &str) -> Option<PathBuf> {
        for folder in &self.folders {
            if let Some(path) = Self::resolve_skill_path(folder, name).await {
                return Some(path);
            }
        }
        None
    }

    /// Resolve skill path by name: folder/{name}/SKILL.md
    /// Supports colon-separated namespaces: "a:b" → folder/a/b/SKILL.md
    async fn resolve_skill_path(folder: &Path, name: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = name.split(':').collect();
        let skill_path = folder
            .join(parts.iter().collect::<std::path::PathBuf>())
            .join("SKILL.md");

        if tokio::fs::try_exists(&skill_path).await.unwrap_or(false) {
            skill_path.canonicalize().ok().or(Some(skill_path))
        } else {
            None
        }
    }

    /// Read skill file content asynchronously.
    pub async fn read_skill_content(path: &Path) -> Result<String> {
        tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read skill file: {}", path.display()))
    }

    // ── Plugin support (yomi pattern) ────────────────────────────────────────

    /// Load skills from a plugin descriptor.
    pub fn load_from_plugin(plugin: &Plugin) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();

        if let Some(ref skills_path) = plugin.skills_path {
            Self::load_plugin_skills_dir(skills_path, &plugin.name, &mut skills)?;
        }

        for skills_path in &plugin.skills_paths {
            Self::load_plugin_skills_dir(skills_path, &plugin.name, &mut skills)?;
        }

        Ok(skills)
    }

    fn load_plugin_skills_dir(
        skills_path: &Path,
        plugin_name: &str,
        skills: &mut Vec<Skill>,
    ) -> Result<()> {
        if !skills_path.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(skills_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists() {
                    if let Ok(skill) = Self::load_plugin_skill(&skill_file, plugin_name) {
                        skills.push(skill);
                    }
                }
            }
        }
        Ok(())
    }

    fn load_plugin_skill(path: &Path, plugin_name: &str) -> Result<Skill> {
        use std::io::{BufRead, BufReader};

        let skill_dir_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid skill path: {}", path.display()))?;
        let skill_name = format!("{plugin_name}:{skill_dir_name}");

        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open skill file: {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let first_line = lines.next().transpose()?;
        if first_line.as_deref() != Some("---") {
            anyhow::bail!("Skill file must start with frontmatter delimiter ---");
        }

        let mut yaml_lines = Vec::new();
        let mut found_end = false;
        for line in lines {
            let line = line?;
            if line == "---" {
                found_end = true;
                break;
            }
            yaml_lines.push(line);
        }

        if !found_end {
            anyhow::bail!("Frontmatter end delimiter not found");
        }

        let yaml_content = yaml_lines.join("\n");
        let frontmatter: SkillFrontmatter = serde_yaml::from_str(&yaml_content)
            .context("Failed to parse skill frontmatter YAML")?;

        Ok(Skill {
            name: skill_name,
            description: frontmatter.description,
            triggers: frontmatter.triggers,
            source_path: path.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_output_short() {
        let short = "hello world";
        assert_eq!(truncate_output(short), short);
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "x".repeat(50_000);
        let truncated = truncate_output(&long);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with("[Output truncated due to length.]"));
    }

    #[test]
    fn test_derive_skill_name() {
        let root = Path::new("/root/skills");
        let path = Path::new("/root/skills/debug/SKILL.md");
        assert_eq!(SkillLoader::derive_skill_name(path, root), "debug");
    }

    #[test]
    fn test_derive_skill_name_nested() {
        let root = Path::new("/root/skills");
        let path = Path::new("/root/skills/a/b/SKILL.md");
        assert_eq!(SkillLoader::derive_skill_name(path, root), "a:b");
    }
}
