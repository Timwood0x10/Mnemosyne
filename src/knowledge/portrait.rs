//! Deterministic resume/person-document portrait extraction.
//!
//! Turns plain text (typically extracted from a resume PDF via
//! [`crate::knowledge::pdf`]) into a structured [`PersonPortrait`] without an
//! LLM: name, position, contacts, links, skills, and projects are extracted
//! by stable, rule-based heuristics. This is the "document → portrait" path
//! of the external-knowledge plan (对话走认知 Facts，文档走结构化提取).
//!
//! ## Extraction rules (deterministic)
//!
//! - `name`     — first non-empty line, "·简历/简历" suffix stripped.
//! - `position` — first line containing an occupation keyword.
//! - `contact`  — every line containing `|` or `@` (email / phone).
//! - `links`    — every line containing `GitHub`/`博客`/`blog`/`http`.
//! - `skills`   — lines between the「技术栈」marker and the「项目」section.
//! - `projects` — lines containing `—`, split into title (before) and the
//!   following line as the description.

use serde::{Deserialize, Serialize};

/// A single project extracted from the resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// Project title, e.g. `ARES` from `ARES—Multi-AgentFramework`.
    pub name: String,
    /// The line immediately following the title (best-effort description).
    pub description: String,
}

/// Structured person portrait extracted from resume plain text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonPortrait {
    /// Person name (first non-empty line, resume suffix stripped).
    pub name: String,
    /// Job title / position.
    pub position: String,
    /// Contact lines (email / phone).
    pub contact: Vec<String>,
    /// Link lines (GitHub / blog / URLs).
    pub links: Vec<String>,
    /// Skill lines between the tech-stack marker and the project section.
    pub skills: Vec<String>,
    /// Projects found by the `—` delimiter rule.
    pub projects: Vec<Project>,
}

/// Errors produced during portrait extraction.
#[derive(Debug, thiserror::Error)]
pub enum PortraitError {
    /// Input had no extractable non-empty lines.
    #[error("portrait input is empty (no extractable lines)")]
    EmptyInput,
    /// Input parsed but contained no project section marker (`—`).
    #[error("no project section marker (`—`) found in resume text")]
    MissingProjects,
}

/// Deterministic, rule-based portrait extractor.
#[derive(Debug, Clone, Default)]
pub struct PortraitExtractor;

impl PortraitExtractor {
    /// Create a new extractor (stateless; rules are baked in).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Extract a [`PersonPortrait`] from resume plain text.
    ///
    /// # Errors
    ///
    /// - [`PortraitError::EmptyInput`] when `text` has no non-empty lines.
    /// - [`PortraitError::MissingProjects`] when no `—` project marker exists.
    pub fn extract(&self, text: &str) -> Result<PersonPortrait, PortraitError> {
        let lines: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if lines.is_empty() {
            return Err(PortraitError::EmptyInput);
        }

        let name = strip_resume_suffix(lines.first().map_or("", String::as_str)).to_string();
        let position = lines
            .iter()
            .find(|line| {
                line.contains("工程师")
                    || line.contains("开发者")
                    || line.contains("架构师")
                    || line.contains("研究员")
                    || line.contains("Engineer")
                    || line.contains("Developer")
            })
            .cloned()
            .unwrap_or_default();

        let contact: Vec<String> = lines
            .iter()
            .filter(|line| line.contains('|') || line.contains('@'))
            .cloned()
            .collect();
        let links: Vec<String> = lines
            .iter()
            .filter(|line| {
                line.contains("GitHub")
                    || line.contains("博客")
                    || line.contains("blog")
                    || line.contains("http")
            })
            .cloned()
            .collect();

        let skills = extract_skills(&lines);
        let projects = extract_projects(&lines)?;

        Ok(PersonPortrait {
            name,
            position,
            contact,
            links,
            skills,
            projects,
        })
    }
}

/// Strip a trailing「·简历」/「简历」suffix from a name line.
fn strip_resume_suffix(line: &str) -> &str {
    line.trim_end_matches("·简历")
        .trim_end_matches("简历")
        .trim_end_matches("·Resume")
        .trim_end_matches("Resume")
        .trim()
}

/// Collect skill lines: everything after the「技术栈」marker and before the
/// first project title line (which contains `—` or a known project name).
fn extract_skills(lines: &[String]) -> Vec<String> {
    let stack_start = lines.iter().position(|line| {
        line.contains("技术栈") || line.contains("Skills") || line.contains("技能")
    });
    let Some(stack_start) = stack_start else {
        return Vec::new();
    };
    let section_end = lines[stack_start + 1..]
        .iter()
        .position(|line| line.contains('—') || line.contains("项目"))
        .map(|p| stack_start + 1 + p)
        .unwrap_or(lines.len());
    lines[stack_start + 1..section_end].to_vec()
}

/// Extract projects: lines containing `—` become titles; the following line
/// (when it exists and is not itself a title) is the description.
fn extract_projects(lines: &[String]) -> Result<Vec<Project>, PortraitError> {
    let mut projects = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((title, _)) = line.split_once('—') {
            let name = title.trim().to_string();
            let description = lines
                .get(i + 1)
                .map(String::as_str)
                .filter(|next| !next.contains('—'))
                .unwrap_or("")
                .to_string();
            projects.push(Project { name, description });
        }
    }
    if projects.is_empty() {
        return Err(PortraitError::MissingProjects);
    }
    Ok(projects)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESUME_ZH: &str = "\
师琤琤·简历

师琤琤

AIAgent 研发工程师

| scchain1998@163.com | 1.9935725605
GitHub:Timwood0x10 | 博客:timwood0x10.github.io/myblog

技术栈

语言与框架：Rust/Go/Python/Zig
AI/Agent：MemoryDistillation · RAG 检索增强

项目

ARES—Multi-AgentFramework
独立设计并实现生产级 Go 多智能体运行时。
OmniScope—LLVMIR跨语言内存安全分析框架
获 Rust 官方社区收录。";

    /// Objective: Verify a full Chinese resume extracts every portrait field.
    /// Invariants: name/position/contact/links/skills/projects are all
    /// populated with the exact expected values.
    #[test]
    fn extracts_full_portrait_from_resume() {
        let extractor = PortraitExtractor::new();
        let portrait = extractor.extract(RESUME_ZH).expect("resume must extract");

        assert_eq!(portrait.name, "师琤琤", "name must strip the ·简历 suffix");
        assert!(
            portrait.position.contains("研发工程师"),
            "position must carry the occupation, got {:?}",
            portrait.position
        );
        assert!(
            portrait
                .contact
                .iter()
                .any(|c| c.contains("scchain1998@163.com")),
            "email must be collected into contact"
        );
        assert!(
            portrait.links.iter().any(|l| l.contains("GitHub")),
            "GitHub link must be collected"
        );
        assert!(
            portrait.skills.iter().any(|s| s.contains("Rust/Go/Python")),
            "tech-stack skills must be extracted"
        );
        assert_eq!(portrait.projects.len(), 2, "two `—` projects expected");
        assert_eq!(portrait.projects[0].name, "ARES");
        assert_eq!(
            portrait.projects[0].description, "独立设计并实现生产级 Go 多智能体运行时。",
            "project description must be the line after the title"
        );
    }

    /// Objective: Verify the name line has its resume suffix stripped.
    /// Invariants: "张三·简历" → "张三"; "李四简历" → "李四".
    #[test]
    fn name_strips_resume_suffix() {
        assert_eq!(strip_resume_suffix("张三·简历"), "张三");
        assert_eq!(strip_resume_suffix("李四简历"), "李四");
        assert_eq!(strip_resume_suffix("王五"), "王五", "plain name unchanged");
    }

    /// Objective: Verify empty and whitespace-only input returns a typed error.
    /// Invariants: empty string and spaces → Err(PortraitError::EmptyInput),
    /// never a panic and never a silent empty portrait.
    #[test]
    fn empty_input_errors() {
        let extractor = PortraitExtractor::new();
        for bad in ["", "   ", "\n\n\t\n"] {
            let err = extractor.extract(bad).unwrap_err();
            assert!(
                matches!(err, PortraitError::EmptyInput),
                "empty input must yield EmptyInput, got {err:?}"
            );
        }
    }

    /// Objective: Verify input without any project marker returns a typed error.
    /// Invariants: name-only resume → Err(PortraitError::MissingProjects).
    #[test]
    fn missing_projects_errors() {
        let extractor = PortraitExtractor::new();
        let err = extractor.extract("张三\n工程师\n").unwrap_err();
        assert!(
            matches!(err, PortraitError::MissingProjects),
            "resume without `—` must yield MissingProjects, got {err:?}"
        );
    }

    /// Objective: Verify a project title and its following description split.
    /// Invariants: "ARES—Multi-AgentFramework" + next line → name="ARES",
    /// description=next line; a title followed by another title keeps an empty
    /// description instead of stealing the next title.
    #[test]
    fn projects_split_name_and_description() {
        let lines = vec![
            "ARES—Multi-AgentFramework".to_string(),
            "独立设计并实现。".to_string(),
            "OmniScope—LLVMIR".to_string(),
            "SecondTitle—x".to_string(),
        ];
        let projects = extract_projects(&lines).expect("projects extract");
        assert_eq!(projects[0].name, "ARES");
        assert_eq!(projects[0].description, "独立设计并实现。");
        assert_eq!(projects[1].name, "OmniScope");
        assert_eq!(projects[2].name, "SecondTitle");
        assert!(
            projects[2].description.is_empty(),
            "a title followed by another title must not steal it as description"
        );
    }

    /// Objective: Verify an English resume extracts the same fields.
    /// Invariants: Name/Engineer/Email/GitHub/projects (`—`) are all found.
    #[test]
    fn english_resume_extracts() {
        let text = "\
Jane Doe

Senior Software Engineer

| jane@example.com | 555-0100
GitHub:janedoe

Skills

Rust, Go, distributed systems

Projects

Core—Distributed Runtime
A production-grade runtime.
";
        let extractor = PortraitExtractor::new();
        let portrait = extractor
            .extract(text)
            .expect("english resume must extract");

        assert_eq!(portrait.name, "Jane Doe");
        assert!(
            portrait.position.contains("Engineer"),
            "position must match the Engineer keyword"
        );
        assert!(
            portrait
                .contact
                .iter()
                .any(|c| c.contains("jane@example.com")),
            "email must extract"
        );
        assert_eq!(portrait.projects[0].name, "Core");
    }
}
