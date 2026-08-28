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

/// A document lifecycle reaction: on a doc event, notify a role + action.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocReaction {
    /// Trigger event name (`created` / `updated` / `reviewed` / ...).
    pub on: String,
    /// Optional target role ("通知 <角色>"); None = broadcast / agent decides.
    pub to_role: Option<String>,
    /// Human-readable action description (executed by the agent).
    pub action: String,
}

/// A document type declared in the `## 文档` section of TEAM.md.
///
/// This is a *declarative contract*: the state machine (`states`) is dynamic —
/// it comes from TEAM.md, not from `state.rs`. Different teams can define
/// different documents and flows without code changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocSpec {
    /// The `### key` — unique document identifier (referenced by events).
    /// JSON snapshots (T2 `_spec`) historically used `doc` as the field name;
    /// accept it as an alias when loading.
    #[serde(alias = "doc")]
    pub key: String,
    /// Display title (`标题` / `title`); falls back to `key`.
    pub title: String,
    /// Purpose (`用途` / `purpose`).
    pub purpose: Option<String>,
    /// Required sections (`模板` / `template`); empty = no template, free-form.
    pub template: Vec<String>,
    /// Roles allowed to create this document (`创建者` / `creators`); empty = anyone.
    pub creators: Vec<String>,
    /// Owning role (`所有者` / `owner`) — default receiver of changes.
    pub owner: String,
    /// Roles that may advance state (`审批者` / `approvers`); empty = owner only.
    pub approvers: Vec<String>,
    /// Declared state chain (`状态流` / `states`), e.g. `draft -> review -> approved -> done`.
    pub states: Vec<String>,
    /// Reactions to lifecycle events (`变更响应` / `reactions`).
    pub reactions: Vec<DocReaction>,
}

impl DocSpec {
    fn from_key(key: &str) -> Self {
        DocSpec {
            key: key.to_string(),
            title: key.to_string(),
            ..Default::default()
        }
    }

    /// Whether the doc is missing mandatory fields (`owner`, `states`).
    /// Such docs are parsed but skipped at bootstrap instantiation.
    pub fn is_incomplete(&self) -> bool {
        self.owner.is_empty() || self.states.is_empty()
    }
}

/// The parsed `.teamx/TEAM.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamFile {
    pub team_name: String,
    pub background: Option<String>,
    pub goals: Vec<String>,
    pub members: Vec<MemberProfile>,
    /// Document contracts declared in the `## 文档` section.
    pub docs: Vec<DocSpec>,
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
    v.split([',', '，', '、', ';', '；', '|'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Doc-section field aliases (Chinese + English keys). English aliases are
/// matched case-insensitively (`Title:` / `title:` / `TITLE:` all work).
fn doc_field_name(trimmed: &str) -> Option<(&'static str, &str)> {
    for (canon, aliases) in [
        ("title", &["标题", "title"][..]),
        ("purpose", &["用途", "purpose", "目的"][..]),
        ("template", &["模板", "template"][..]),
        ("creators", &["创建者", "creators", "创建"][..]),
        ("owner", &["所有者", "owner", "负责人"][..]),
        ("approvers", &["审批者", "approvers", "审批"][..]),
        ("states", &["状态流", "states", "状态"][..]),
        ("reactions", &["变更响应", "reactions", "响应"][..]),
    ] {
        for a in aliases {
            // English aliases: case-insensitive prefix; Chinese: exact prefix.
            let matched = if a.is_ascii() {
                trimmed.get(..a.len()).map(|p| p.eq_ignore_ascii_case(a)).unwrap_or(false)
            } else {
                trimmed.starts_with(a)
            };
            if matched {
                let rest = trimmed[a.len()..].trim();
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

/// Split a doc state chain `draft -> review -> approved -> done` into states.
/// Accepts `->`, `→`, or list separators.
fn split_states(v: &str) -> Vec<String> {
    v.split("->")
        .flat_map(|s| s.split('→'))
        .flat_map(|s| s.split([',', '，']))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Strip surrounding `[` / `]` from a role list element (e.g. `[pm]`).
fn strip_brackets(s: &str) -> String {
    s.trim().trim_start_matches('[').trim_end_matches(']').trim().to_string()
}

/// Normalize a doc list field (template/creators/approvers): split + strip
/// brackets from each element, dropping empties (`[]` yields an empty list).
fn doc_list(value: &str) -> Vec<String> {
    split_list(value)
        .into_iter()
        .map(|s| strip_brackets(&s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A member key doubles as a directory name under `.teamx/members/`, so it
/// must never be empty/hidden or contain path separators / control characters.
/// Non-ASCII keys (e.g. `小明`) are fine and remain allowed.
fn is_safe_member_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('.')
        && !key
            .chars()
            .any(|c| matches!(c, '/' | '\\') || c.is_control())
}

/// Parse TEAM.md text. Returns an Err only when the file is unreadable or
/// empty (or a member key is unsafe — it is used as a path component);
/// structural looseness (missing sections/fields) never fails.
pub fn parse_team_file_text(text: &str) -> Result<TeamFile, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("TEAM.md is empty".to_string());
    }
    let mut tf = TeamFile::default();
    // Current section: "root" | "background" | "goals" | "members" | "docs"
    let mut section = "root".to_string();
    let mut member: Option<MemberProfile> = None;
    let mut doc: Option<DocSpec> = None;
    // Whether the previous line opened a `- 变更响应:` block (nested `on ...`
    // sub-items are reactions). Reset on any non-nested field line.
    let mut reactions_open = false;

    let flush_member = |tf: &mut TeamFile, m: &Option<MemberProfile>| {
        if let Some(m) = m {
            if !m.key.is_empty() {
                tf.members.push(m.clone());
            }
        }
    };
    let flush_doc = |tf: &mut TeamFile, d: &Option<DocSpec>| {
        if let Some(d) = d {
            if !d.key.is_empty() {
                tf.docs.push(d.clone());
            }
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Subsection header: `### key` — a member (under `## 成员`) or a
        // document spec (under `## 文档`).
        if let Some(key) = trimmed.strip_prefix("### ") {
            let key = key.trim();
            if !is_safe_member_key(key) {
                return Err(format!(
                    "unsafe TEAM.md member key `{key}`: no path separators, control chars or \
                     leading dot (the key becomes a directory name)"
                ));
            }
            if section == "docs" {
                flush_doc(&mut tf, &doc);
                doc = Some(DocSpec::from_key(key));
                reactions_open = false;
            } else {
                flush_member(&mut tf, &member);
                member = Some(MemberProfile::from_key(key));
            }
            continue;
        }
        // Section header: `## name`
        if let Some(name) = trimmed.strip_prefix("## ") {
            flush_member(&mut tf, &member);
            member = None;
            flush_doc(&mut tf, &doc);
            doc = None;
            reactions_open = false;
            section = match name.trim() {
                "背景" | "Background" | "背景信息" => "background".to_string(),
                "目标" | "目标与范围" | "Goals" | "Goal" => "goals".to_string(),
                "成员" | "团队成员" | "Members" | "Team" => "members".to_string(),
                "文档" | "Docs" | "Documents" | "Document" => "docs".to_string(),
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
            "docs" => {
                if let Some(d) = doc.as_mut() {
                    // A nested reaction sub-item: `- on <event>: <action>`.
                    // Recognized when the reactions block is open AND the line
                    // is indented (or already starts with `on`).
                    let indented = line.starts_with(' ') || line.starts_with('\t');
                    let fld = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim();
                    if reactions_open && (indented || fld.starts_with("on ")) {
                        if let Some(rest) = fld.strip_prefix("on ") {
                            if let Some((ev, act)) = rest.split_once(':') {
                                let ev = ev.trim();
                                let act = act.trim();
                                if !ev.is_empty() {
                                    let (to_role, action) = parse_reaction(act);
                                    d.reactions.push(DocReaction {
                                        on: ev.to_string(),
                                        to_role,
                                        action,
                                    });
                                }
                            }
                        }
                        continue;
                    }
                    let fld = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim();
                    if let Some((canon, value)) = doc_field_name(fld) {
                        match canon {
                            "title" => {
                                if !value.is_empty() {
                                    d.title = value.to_string();
                                }
                            }
                            "purpose" => {
                                if !value.is_empty() {
                                    d.purpose = Some(value.to_string());
                                }
                            }
                            "template" => d.template = doc_list(value),
                            "creators" => d.creators = doc_list(value),
                            "owner" => {
                                if !value.is_empty() {
                                    d.owner = value.to_string();
                                }
                            }
                            "approvers" => d.approvers = doc_list(value),
                            "states" => d.states = split_states(value),
                            "reactions" => {
                                // `- 变更响应:` (possibly with empty value):
                                // following nested `on ...` items are reactions.
                                reactions_open = true;
                            }
                            _ => {}
                        }
                    } else {
                        reactions_open = false;
                    }
                }
            }
            _ => {}
        }
    }
    flush_member(&mut tf, &member);
    flush_doc(&mut tf, &doc);
    Ok(tf)
}

/// Parse a reaction action string into (optional target role, action).
/// Supports `通知 <角色> <动作>` / `notify <role> <action>` prefixes.
fn parse_reaction(act: &str) -> (Option<String>, String) {
    for prefix in ["通知", "notify"] {
        if let Some(rest) = act.strip_prefix(prefix) {
            let rest = rest.trim();
            if let Some(space) = rest.find(char::is_whitespace) {
                let role = rest[..space].trim();
                let action = rest[space..].trim();
                if !role.is_empty() {
                    return (Some(role.to_string()), action.to_string());
                }
            } else if !rest.is_empty() {
                return (Some(rest.to_string()), String::new());
            }
        }
    }
    (None, act.to_string())
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
    fn member_key_traversal_rejected() {
        for key in ["../../evil", "..", ".hidden", "a/b", "a\\b"] {
            let text = format!("# T\n## 成员\n### {key}\n姓名: x\n");
            let err = parse_team_file_text(&text).unwrap_err();
            assert!(err.contains("unsafe TEAM.md member key"), "{key}: {err}");
        }
        // non-ASCII keys (e.g. 小明) are fine — they contain no separators
        let tf = parse_team_file_text("# T\n## Members\n### 小明\n").unwrap();
        assert_eq!(tf.members[0].key, "小明");
    }

    #[test]
    fn member_key_dotted_ok() {
        let tf = parse_team_file_text("# T\n## Members\n### alice.dev_2\nname: Alice\n").unwrap();
        assert_eq!(tf.members[0].key, "alice.dev_2");
        assert_eq!(tf.members[0].display_name, "Alice");
    }

    #[test]
    fn goals_multiline_and_list_separators() {
        let tf = parse_team_file_text("# T\n## 目标\n- a\n- b\n- c\n## 成员\n### x\n技能: a、b，c; d；e\n").unwrap();
        assert_eq!(tf.goals, vec!["a", "b", "c"]);
        assert_eq!(tf.members[0].skills, vec!["a", "b", "c", "d", "e"]);
    }

    // ------------------------------------------------------------------
    // `## 文档` (Document) section
    // ------------------------------------------------------------------

    const DOCS: &str = r#"# 企业数字化平台

## 文档

### requirements
- 标题: 需求文档
- 用途: 定义产品需求与验收标准
- 模板: 背景 | 目标 | 用户故事 | 验收标准
- 创建者: [pm]
- 所有者: pm
- 审批者: [reviewer, owner]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知 pm 细化需求
    - on updated: 通知 reviewer 复审
    - on approved: 通知 ui-dev 与 java-dev 开始设计

### issue
- 标题: 缺陷 / 改进请求
- 所有者: team-lead
- 状态流: opened -> triaged -> assigned -> fixing -> verified -> closed
- 变更响应:
    - on created: team-lead 分析并分诊（triage）
    - on verified: 通知 owner 关闭并记录 release-note
"#;

    #[test]
    fn parse_docs_section() {
        let tf = parse_team_file_text(DOCS).unwrap();
        assert_eq!(tf.docs.len(), 2);

        // requirements — full spec
        let req = &tf.docs[0];
        assert_eq!(req.key, "requirements");
        assert_eq!(req.title, "需求文档");
        assert_eq!(req.purpose.as_deref(), Some("定义产品需求与验收标准"));
        assert_eq!(req.template, vec!["背景", "目标", "用户故事", "验收标准"]);
        assert_eq!(req.creators, vec!["pm"]);
        assert_eq!(req.owner, "pm");
        assert_eq!(req.approvers, vec!["reviewer", "owner"]);
        assert_eq!(req.states, vec!["draft", "review", "approved", "done"]);
        assert!(!req.is_incomplete());
        assert_eq!(req.reactions.len(), 3);
        assert_eq!(req.reactions[0].on, "created");
        assert_eq!(req.reactions[0].to_role.as_deref(), Some("pm"));
        assert_eq!(req.reactions[0].action, "细化需求");
        assert_eq!(req.reactions[1].to_role.as_deref(), Some("reviewer"));
        assert_eq!(req.reactions[2].to_role.as_deref(), Some("ui-dev"));
        assert_eq!(req.reactions[2].action, "与 java-dev 开始设计");

        // issue — minimal fields
        let iss = &tf.docs[1];
        assert_eq!(iss.key, "issue");
        assert_eq!(iss.title, "缺陷 / 改进请求");
        assert!(iss.template.is_empty()); // no template -> free-form
        assert!(iss.creators.is_empty()); // empty -> anyone
        assert_eq!(iss.owner, "team-lead");
        assert_eq!(iss.states, vec!["opened", "triaged", "assigned", "fixing", "verified", "closed"]);
        assert_eq!(iss.reactions.len(), 2);
        // no `通知` prefix -> to_role None
        assert_eq!(iss.reactions[0].to_role, None);
        assert_eq!(iss.reactions[0].action, "team-lead 分析并分诊（triage）");
        assert_eq!(iss.reactions[1].to_role.as_deref(), Some("owner"));
    }

    #[test]
    fn docs_english_aliases_and_separators() {
        let tf = parse_team_file_text(
            "# T\n## Docs\n### pr\nTitle: 代码合并请求\nPurpose: 评审与合并\n\
             Template: 变更描述, 关联 issue\nCreators: contributor, developer\n\
             Owner: 提交者\nApprovers: reviewer\nStates: opened -> reviewing -> approved -> merged\n\
             Reactions:\n    - on created: notify reviewer 评审\n",
        )
        .unwrap();
        assert_eq!(tf.docs.len(), 1);
        let pr = &tf.docs[0];
        assert_eq!(pr.title, "代码合并请求");
        assert_eq!(pr.template, vec!["变更描述", "关联 issue"]);
        assert_eq!(pr.creators, vec!["contributor", "developer"]);
        assert_eq!(pr.states, vec!["opened", "reviewing", "approved", "merged"]);
        assert_eq!(pr.reactions.len(), 1);
        assert_eq!(pr.reactions[0].on, "created");
        assert_eq!(pr.reactions[0].to_role.as_deref(), Some("reviewer"));
        assert_eq!(pr.reactions[0].action, "评审");
    }

    #[test]
    fn docs_fullwidth_colon_and_arrow() {
        let tf = parse_team_file_text(
            "# T\n## 文档\n### design\n标题：概要设计\n所有者：架构师\n状态流：draft → review → approved\n",
        )
        .unwrap();
        let d = &tf.docs[0];
        assert_eq!(d.title, "概要设计");
        assert_eq!(d.owner, "架构师");
        assert_eq!(d.states, vec!["draft", "review", "approved"]);
    }

    #[test]
    fn docs_incomplete_missing_owner_or_states() {
        // missing owner + states -> incomplete (parses but flagged)
        let tf = parse_team_file_text("# T\n## 文档\n### x\n标题: X\n").unwrap();
        assert_eq!(tf.docs.len(), 1);
        assert!(tf.docs[0].is_incomplete());
        // missing states only
        let tf = parse_team_file_text("# T\n## 文档\n### y\n所有者: pm\n").unwrap();
        assert!(tf.docs[0].is_incomplete());
        // complete
        let tf = parse_team_file_text("# T\n## 文档\n### z\n所有者: pm\n状态流: a -> b\n").unwrap();
        assert!(!tf.docs[0].is_incomplete());
    }

    #[test]
    fn docs_no_reactions_block_ok() {
        let tf = parse_team_file_text("# T\n## 文档\n### a\n所有者: pm\n状态流: a -> b\n").unwrap();
        assert!(tf.docs[0].reactions.is_empty());
    }

    #[test]
    fn docs_and_members_coexist() {
        let tf = parse_team_file_text(
            "# T\n## 成员\n### 小明\n角色: contributor\n## 文档\n### issue\n所有者: lead\n状态流: a -> b\n",
        )
        .unwrap();
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.docs.len(), 1);
        assert_eq!(tf.members[0].key, "小明");
        assert_eq!(tf.docs[0].key, "issue");
    }

    #[test]
    fn docs_key_traversal_rejected() {
        for key in ["../../evil", "..", ".hidden", "a/b", "a\\b"] {
            let text = format!("# T\n## 文档\n### {key}\n所有者: pm\n");
            let err = parse_team_file_text(&text).unwrap_err();
            assert!(err.contains("unsafe TEAM.md member key"), "{key}: {err}");
        }
    }
}
