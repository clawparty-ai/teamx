//! teamfile.rs — parser for `.teamx/TEAM.md`, the team definition file.
//!
//! `team create` detects this file at the project root and uses it to
//! auto-initialize the team: parse the team name, background, goals and per
//! member profiles (role / duties / skills / outputs), then generate goal,
//! invitation letters, member AGENTS.md and work directories.
//!
//! Format (Markdown-ish, loosely parsed):
//!
//! ```md
//! # Team Name
//!
//! ## 背景
//! ...free text...
//!
//! ## 目标
//! - goal one
//! - goal two
//!
//! ## 成员
//! ### owner
//! - 姓名: 企业数字化平台
//! - 角色: owner
//! - 分工: 架构设计、代码审查
//! - 技能: Rust, TypeScript
//! - 输出: 架构文档、核心代码
//! ```

use std::fs;
use std::path::Path;

/// A member profile parsed from a `### key` section of TEAM.md.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberProfile {
    /// The `### key` — used as the member directory name.
    pub key: String,
    /// display_name (from `姓名` / `名字` / `name`), falls back to `key`.
    pub display_name: String,
    /// role key (`角色` / `role`).
    pub role: Option<String>,
    /// duties / role description (`分工` / `description`).
    pub description: Option<String>,
    /// skills (`技能` / `skills`), comma separated.
    pub skills: Vec<String>,
    /// work outputs (`输出` / `outputs`), comma separated.
    pub outputs: Vec<String>,
}

/// The parsed `.teamx/TEAM.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamFile {
    pub team_name: String,
    pub background: Option<String>,
    pub goals: Vec<String>,
    pub members: Vec<MemberProfile>,
}

impl MemberProfile {
    fn from_key(key: &str) -> Self {
        MemberProfile {
            key: key.to_string(),
            display_name: key.to_string(),
            ..Default::default()
        }
    }
}

/// Field aliases accepted in a member section (Chinese + English keys).
/// Returns the canonical field name when the line matches, else None.
fn field_name(trimmed: &str) -> Option<(&'static str, &str)> {
    for (canon, aliases) in [
        ("display_name", &["姓名", "名字", "name", "显示名"][..]),
        ("role", &["角色", "role"][..]),
        ("description", &["分工", "职责", "description", "duties"][..]),
        ("skills", &["技能", "skills"][..]),
        ("outputs", &["输出", "产出", "outputs", "deliverables"][..]),
    ] {
        for a in aliases {
            if let Some(rest) = trimmed.strip_prefix(a) {
                let rest = rest.trim();
                // the separator can be `:` or `：` (fullwidth colon)
                if let Some(v) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：')) {
                    let v = v.trim();
                    if v.is_empty() {
                        return Some((canon, ""));
                    }
                    return Some((canon, v));
                }
            }
        }
    }
    None
}

fn split_list(v: &str) -> Vec<String> {
    v.split([',', '，', '、', ';', '；'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse TEAM.md text. Returns an Err only when the file is unreadable or
/// empty; structural looseness (missing sections/fields) never fails.
pub fn parse_team_file_text(text: &str) -> Result<TeamFile, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("TEAM.md is empty".to_string());
    }
    let mut tf = TeamFile::default();
    // Current section: "root" | "background" | "goals" | "members"
    let mut section = "root".to_string();
    let mut member: Option<MemberProfile> = None;

    let flush_member = |tf: &mut TeamFile, m: &Option<MemberProfile>| {
        if let Some(m) = m {
            if !m.key.is_empty() {
                tf.members.push(m.clone());
            }
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Member subsection header: `### key`
        if let Some(key) = trimmed.strip_prefix("### ") {
            flush_member(&mut tf, &member);
            member = Some(MemberProfile::from_key(key.trim()));
            continue;
        }
        // Section header: `## name`
        if let Some(name) = trimmed.strip_prefix("## ") {
            flush_member(&mut tf, &member);
            member = None;
            section = match name.trim() {
                "背景" | "Background" | "背景信息" => "background".to_string(),
                "目标" | "目标与范围" | "Goals" | "Goal" => "goals".to_string(),
                "成员" | "团队成员" | "Members" | "Team" => "members".to_string(),
                _ => "root".to_string(),
            };
            continue;
        }
        // H1 title: `# name` (only the first one)
        if section == "root" && tf.team_name.is_empty() {
            if let Some(name) = trimmed.strip_prefix("# ") {
                tf.team_name = name.trim().to_string();
                continue;
            }
        }
        match section.as_str() {
            "background" => {
                let v = trimmed.trim();
                if !v.is_empty() {
                    let prev = tf.background.take().unwrap_or_default();
                    tf.background = Some(if prev.is_empty() { v.to_string() } else { format!("{prev} {v}") });
                }
            }
            "goals" => {
                if let Some(g) = trimmed.strip_prefix("- ") {
                    let g = g.trim();
                    if !g.is_empty() {
                        tf.goals.push(g.to_string());
                    }
                }
            }
            "members" => {
                if let Some(m) = member.as_mut() {
                    // Member field lines may be list items: `- 姓名: ...`
                    let fld = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim();
                    if let Some((canon, value)) = field_name(fld) {
                        match canon {
                            "display_name" => {
                                if !value.is_empty() {
                                    m.display_name = value.to_string();
                                }
                            }
                            "role" => {
                                if !value.is_empty() {
                                    m.role = Some(value.to_string());
                                }
                            }
                            "description" => {
                                if !value.is_empty() {
                                    m.description = Some(value.to_string());
                                }
                            }
                            "skills" => m.skills = split_list(value),
                            "outputs" => m.outputs = split_list(value),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    flush_member(&mut tf, &member);
    Ok(tf)
}

/// Read and parse `.teamx/TEAM.md` under the project root. Returns Ok(None)
/// when the file does not exist.
pub fn load_team_file(project_root: &Path) -> Result<Option<TeamFile>, String> {
    let path = project_root.join(".teamx").join("TEAM.md");
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_team_file_text(&text).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"# 企业数字化平台

## 背景
围绕 Team Goal 构建团队协作平台：支持任务分派、reverse tunnel、活动分析。

## 目标
- 8 月底交付 v1.0
- 支持跨网络 reverse tunnel
- 提供活动/成本分析

## 成员
### owner
- 姓名: 企业数字化平台
- 角色: owner
- 分工: 架构设计、代码审查
- 技能: Rust, TypeScript, 系统设计
- 输出: 架构文档、核心代码

### 小明
- 姓名: 小明
- 角色: contributor
- 分工: 前端开发、测试
- 技能: React, TypeScript
- 输出: 看板组件、测试用例
"#;

    #[test]
    fn parse_full_team_file() {
        let tf = parse_team_file_text(FULL).unwrap();
        assert_eq!(tf.team_name, "企业数字化平台");
        assert!(tf.background.unwrap().contains("reverse tunnel"));
        assert_eq!(tf.goals.len(), 3);
        assert_eq!(tf.goals[0], "8 月底交付 v1.0");
        assert_eq!(tf.members.len(), 2);
        let m1 = &tf.members[0];
        assert_eq!(m1.key, "owner");
        assert_eq!(m1.display_name, "企业数字化平台");
        assert_eq!(m1.role.as_deref(), Some("owner"));
        assert_eq!(m1.description.as_deref(), Some("架构设计、代码审查"));
        assert_eq!(m1.skills, vec!["Rust", "TypeScript", "系统设计"]);
        assert_eq!(m1.outputs, vec!["架构文档", "核心代码"]);
        let m2 = &tf.members[1];
        assert_eq!(m2.display_name, "小明");
        assert_eq!(m2.role.as_deref(), Some("contributor"));
    }

    #[test]
    fn english_field_aliases() {
        let tf = parse_team_file_text(
            "# Demo Team\n## Members\n### alice\nname: Alice\nrole: reviewer\ndescription: review code\nskills: rust\noutputs: reports\n",
        )
        .unwrap();
        assert_eq!(tf.team_name, "Demo Team");
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].display_name, "Alice");
        assert_eq!(tf.members[0].role.as_deref(), Some("reviewer"));
        assert_eq!(tf.members[0].skills, vec!["rust"]);
        assert_eq!(tf.members[0].outputs, vec!["reports"]);
    }

    #[test]
    fn fullwidth_colon_separator() {
        let tf = parse_team_file_text("# T\n## 成员\n### bob\n姓名：Bob\n角色：contributor\n").unwrap();
        assert_eq!(tf.members[0].display_name, "Bob");
        assert_eq!(tf.members[0].role.as_deref(), Some("contributor"));
    }

    #[test]
    fn no_members_section_ok() {
        let tf = parse_team_file_text("# T\n## 目标\n- a\n").unwrap();
        assert!(tf.members.is_empty());
        assert_eq!(tf.goals, vec!["a"]);
    }

    #[test]
    fn missing_title_ok() {
        let tf = parse_team_file_text("## 目标\n- x\n").unwrap();
        assert!(tf.team_name.is_empty());
    }

    #[test]
    fn empty_file_errors() {
        assert!(parse_team_file_text("").is_err());
        assert!(parse_team_file_text("   \n  \n").is_err());
    }

    #[test]
    fn member_minimal() {
        let tf = parse_team_file_text("# T\n## 成员\n### 小明\n").unwrap();
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].key, "小明");
        assert_eq!(tf.members[0].display_name, "小明");
        assert!(tf.members[0].role.is_none());
    }

    #[test]
    fn goals_multiline_and_list_separators() {
        let tf = parse_team_file_text("# T\n## 目标\n- a\n- b\n- c\n## 成员\n### x\n技能: a、b，c; d；e\n").unwrap();
        assert_eq!(tf.goals, vec!["a", "b", "c"]);
        assert_eq!(tf.members[0].skills, vec!["a", "b", "c", "d", "e"]);
    }
}
