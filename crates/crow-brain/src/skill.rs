use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A loaded skill with metadata (no body content — injected on-demand per Yomi pattern)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    #[serde(skip)]
    pub source_path: PathBuf,
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
}
