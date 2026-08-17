//! State machines for Team / Member / Goal.
//!
//! Every transition is validated here and must be driven by an event that is
//! appended to the ledger in the same transaction as the projection update.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamState {
    Forming,
    Active,
    Blocked,
    Completed,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberState {
    Pending,
    Active,
    Waiting,
    Idle,
    Left,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalState {
    Proposed,
    Shared,
    Refining,
    InProgress,
    Blocked,
    Achieved,
    Closed,
}

impl TeamState {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamState::Forming => "forming",
            TeamState::Active => "active",
            TeamState::Blocked => "blocked",
            TeamState::Completed => "completed",
            TeamState::Archived => "archived",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "forming" => TeamState::Forming,
            "active" => TeamState::Active,
            "blocked" => TeamState::Blocked,
            "completed" => TeamState::Completed,
            "archived" => TeamState::Archived,
            _ => return None,
        })
    }
}

impl MemberState {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberState::Pending => "pending",
            MemberState::Active => "active",
            MemberState::Waiting => "waiting",
            MemberState::Idle => "idle",
            MemberState::Left => "left",
            MemberState::Denied => "denied",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => MemberState::Pending,
            "active" => MemberState::Active,
            "waiting" => MemberState::Waiting,
            "idle" => MemberState::Idle,
            "left" => MemberState::Left,
            "denied" => MemberState::Denied,
            _ => return None,
        })
    }
}

impl GoalState {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalState::Proposed => "proposed",
            GoalState::Shared => "shared",
            GoalState::Refining => "refining",
            GoalState::InProgress => "in_progress",
            GoalState::Blocked => "blocked",
            GoalState::Achieved => "achieved",
            GoalState::Closed => "closed",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "proposed" => GoalState::Proposed,
            "shared" => GoalState::Shared,
            "refining" => GoalState::Refining,
            "in_progress" => GoalState::InProgress,
            "blocked" => GoalState::Blocked,
            "achieved" => GoalState::Achieved,
            "closed" => GoalState::Closed,
            _ => return None,
        })
    }
}

impl fmt::Display for TeamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl fmt::Display for MemberState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl fmt::Display for GoalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Actions that drive state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Action {
    CreateTeam,
    Join,
    Approve,
    Deny,
    Leave,
    SetRole,
    SetGoal,
    UpdateGoal,
    ShareGoal,
    Ask,
    Respond,
    MemberIdle,
    MemberActive,
    PublishStart,
    PublishProgress,
    PublishDecision,
    PublishBlocked,
    PublishResumed,
    PublishAchieved,
    PublishRefine,
    CloseGoal,
    ArchiveTeam,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Action::CreateTeam => "create_team",
            Action::Join => "join",
            Action::Approve => "approve",
            Action::Deny => "deny",
            Action::Leave => "leave",
            Action::SetRole => "set_role",
            Action::SetGoal => "set_goal",
            Action::UpdateGoal => "update_goal",
            Action::ShareGoal => "share_goal",
            Action::Ask => "ask",
            Action::Respond => "respond",
            Action::MemberIdle => "member_idle",
            Action::MemberActive => "member_active",
            Action::PublishStart => "publish_start",
            Action::PublishProgress => "publish_progress",
            Action::PublishDecision => "publish_decision",
            Action::PublishBlocked => "publish_blocked",
            Action::PublishResumed => "publish_resumed",
            Action::PublishAchieved => "publish_achieved",
            Action::PublishRefine => "publish_refine",
            Action::CloseGoal => "close_goal",
            Action::ArchiveTeam => "archive_team",
        };
        f.write_str(s)
    }
}

pub fn team_transition(from: TeamState, action: &Action) -> Result<TeamState, String> {
    use Action::*;
    Ok(match (from, action) {
        // creation lands in forming (new team); the goal must be shared to go active
        (TeamState::Forming, ShareGoal) => TeamState::Active,
        (TeamState::Active, PublishBlocked) => TeamState::Blocked,
        (TeamState::Blocked, PublishResumed) => TeamState::Active,
        (TeamState::Active, PublishResumed) => TeamState::Active,
        (TeamState::Active, PublishStart) => TeamState::Active,
        (TeamState::Active, PublishProgress) => TeamState::Active,
        (TeamState::Active, PublishDecision) => TeamState::Active,
        (TeamState::Active, PublishRefine) => TeamState::Active,
        (TeamState::Active, PublishAchieved) => TeamState::Active,
        (TeamState::Blocked, PublishStart) => TeamState::Active,
        (TeamState::Blocked, PublishProgress) => TeamState::Blocked,
        (TeamState::Blocked, PublishDecision) => TeamState::Blocked,
        (TeamState::Blocked, PublishRefine) => TeamState::Blocked,
        (TeamState::Blocked, PublishAchieved) => TeamState::Blocked,
        (TeamState::Active, CloseGoal) => TeamState::Completed,
        (TeamState::Blocked, CloseGoal) => TeamState::Completed,
        (TeamState::Completed, ArchiveTeam) => TeamState::Archived,
        (s, a) => {
            return Err(format!(
                "illegal team transition: {s} --{a}-> ?"
            ))
        }
    })
}

pub fn member_transition(from: MemberState, action: &Action) -> Result<MemberState, String> {
    use Action::*;
    Ok(match (from, action) {
        (MemberState::Pending, Approve) => MemberState::Active,
        (MemberState::Pending, Deny) => MemberState::Denied,
        (MemberState::Active, Ask) => MemberState::Waiting,
        (MemberState::Idle, Ask) => MemberState::Waiting,
        (MemberState::Waiting, Respond) => MemberState::Active,
        (MemberState::Active, MemberIdle) => MemberState::Idle,
        (MemberState::Idle, MemberActive) => MemberState::Active,
        (MemberState::Active, MemberActive) => MemberState::Active,
        (MemberState::Active, SetRole) => MemberState::Active,
        // a pending applicant may record their desired role but stays pending
        // until the owner approves (role application, not activation)
        (MemberState::Pending, SetRole) => MemberState::Pending,
        (MemberState::Idle, SetRole) => MemberState::Idle,
        (MemberState::Active, Leave) => MemberState::Left,
        (MemberState::Idle, Leave) => MemberState::Left,
        (MemberState::Waiting, Leave) => MemberState::Left,
        (MemberState::Pending, Leave) => MemberState::Left,
        (s, a) => {
            return Err(format!(
                "illegal member transition: {s} --{a}-> ?"
            ))
        }
    })
}

pub fn goal_transition(from: GoalState, action: &Action) -> Result<GoalState, String> {
    use Action::*;
    Ok(match (from, action) {
        (GoalState::Proposed, ShareGoal) => GoalState::Shared,
        (GoalState::Shared, PublishStart) => GoalState::InProgress,
        (GoalState::Shared, PublishProgress) => GoalState::InProgress,
        (GoalState::Refining, PublishStart) => GoalState::InProgress,
        (GoalState::Refining, PublishProgress) => GoalState::InProgress,
        (GoalState::InProgress, PublishProgress) => GoalState::InProgress,
        (GoalState::InProgress, PublishDecision) => GoalState::InProgress,
        (GoalState::InProgress, PublishStart) => GoalState::InProgress,
        (GoalState::InProgress, PublishBlocked) => GoalState::Blocked,
        (GoalState::InProgress, PublishAchieved) => GoalState::Achieved,
        (GoalState::Blocked, PublishResumed) => GoalState::InProgress,
        (GoalState::Blocked, PublishAchieved) => GoalState::Achieved,
        (GoalState::Blocked, PublishProgress) => GoalState::Blocked,
        (GoalState::Blocked, PublishDecision) => GoalState::Blocked,
        // An `achieved` candidate is not final (it awaits owner verification):
        // the owner may reject it and reopen the goal back into execution
        // (start/resume) or ask for refinement (refine).
        (GoalState::Achieved, PublishStart) => GoalState::InProgress,
        (GoalState::Achieved, PublishResumed) => GoalState::InProgress,
        (GoalState::Achieved, PublishRefine) => GoalState::Refining,
        (GoalState::Shared, PublishRefine) => GoalState::Refining,
        (GoalState::InProgress, PublishRefine) => GoalState::Refining,
        (GoalState::Refining, PublishRefine) => GoalState::Refining,
        (GoalState::Achieved, CloseGoal) => GoalState::Closed,
        (GoalState::InProgress, CloseGoal) => GoalState::Closed,
        (GoalState::Blocked, CloseGoal) => GoalState::Closed,
        (GoalState::Shared, CloseGoal) => GoalState::Closed,
        (GoalState::Proposed, CloseGoal) => GoalState::Closed,
        (s, a) => {
            return Err(format!(
                "illegal goal transition: {s} --{a}-> ?"
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_happy_path() {
        assert_eq!(team_transition(TeamState::Forming, &Action::ShareGoal).unwrap(), TeamState::Active);
        assert_eq!(team_transition(TeamState::Active, &Action::PublishBlocked).unwrap(), TeamState::Blocked);
        assert_eq!(team_transition(TeamState::Blocked, &Action::PublishResumed).unwrap(), TeamState::Active);
        assert_eq!(team_transition(TeamState::Active, &Action::CloseGoal).unwrap(), TeamState::Completed);
        assert_eq!(team_transition(TeamState::Completed, &Action::ArchiveTeam).unwrap(), TeamState::Archived);
    }

    #[test]
    fn team_illegal_transitions_rejected() {
        for (from, action) in [
            (TeamState::Forming, Action::CloseGoal),
            (TeamState::Forming, Action::PublishBlocked),
            (TeamState::Completed, Action::ShareGoal),
            (TeamState::Archived, Action::CloseGoal),
            (TeamState::Archived, Action::ArchiveTeam),
            (TeamState::Active, Action::CreateTeam),
        ] {
            assert!(team_transition(from, &action).is_err(), "{from} --{action}-> ? should be illegal");
        }
        // archive is one-way: completed -> archived is legal, archived is terminal
        assert_eq!(team_transition(TeamState::Completed, &Action::ArchiveTeam).unwrap(), TeamState::Archived);
    }

    #[test]
    fn team_neutral_actions_keep_active() {
        for a in [Action::PublishStart, Action::PublishProgress, Action::PublishDecision, Action::PublishRefine, Action::PublishAchieved] {
            assert_eq!(team_transition(TeamState::Active, &a).unwrap(), TeamState::Active, "{a}");
        }
        // while blocked, progress/decision/achieved do not unblock the team
        for a in [Action::PublishProgress, Action::PublishDecision, Action::PublishAchieved, Action::PublishRefine] {
            assert_eq!(team_transition(TeamState::Blocked, &a).unwrap(), TeamState::Blocked, "{a}");
        }
    }

    #[test]
    fn member_happy_path() {
        assert_eq!(member_transition(MemberState::Pending, &Action::Approve).unwrap(), MemberState::Active);
        assert_eq!(member_transition(MemberState::Pending, &Action::Deny).unwrap(), MemberState::Denied);
        assert_eq!(member_transition(MemberState::Active, &Action::Ask).unwrap(), MemberState::Waiting);
        assert_eq!(member_transition(MemberState::Idle, &Action::Ask).unwrap(), MemberState::Waiting);
        assert_eq!(member_transition(MemberState::Waiting, &Action::Respond).unwrap(), MemberState::Active);
        assert_eq!(member_transition(MemberState::Active, &Action::MemberIdle).unwrap(), MemberState::Idle);
        assert_eq!(member_transition(MemberState::Idle, &Action::MemberActive).unwrap(), MemberState::Active);
    }

    #[test]
    fn member_leave_from_any_state() {
        for from in [MemberState::Pending, MemberState::Active, MemberState::Waiting, MemberState::Idle] {
            assert_eq!(member_transition(from, &Action::Leave).unwrap(), MemberState::Left, "{from}");
        }
        // a left member cannot be re-approved or asked
        assert!(member_transition(MemberState::Left, &Action::Approve).is_err());
        assert!(member_transition(MemberState::Left, &Action::Ask).is_err());
        // a denied applicant cannot be approved later
        assert!(member_transition(MemberState::Denied, &Action::Approve).is_err());
    }

    #[test]
    fn member_role_set_is_neutral() {
        assert_eq!(member_transition(MemberState::Active, &Action::SetRole).unwrap(), MemberState::Active);
        assert_eq!(member_transition(MemberState::Idle, &Action::SetRole).unwrap(), MemberState::Idle);
        // a pending applicant records their desired role but stays pending
        // until the owner approves (approval must not be bypassed)
        assert_eq!(member_transition(MemberState::Pending, &Action::SetRole).unwrap(), MemberState::Pending);
        assert_eq!(member_transition(MemberState::Pending, &Action::Approve).unwrap(), MemberState::Active);
    }

    #[test]
    fn goal_happy_path() {
        assert_eq!(goal_transition(GoalState::Proposed, &Action::ShareGoal).unwrap(), GoalState::Shared);
        assert_eq!(goal_transition(GoalState::Shared, &Action::PublishStart).unwrap(), GoalState::InProgress);
        assert_eq!(goal_transition(GoalState::InProgress, &Action::PublishBlocked).unwrap(), GoalState::Blocked);
        assert_eq!(goal_transition(GoalState::Blocked, &Action::PublishResumed).unwrap(), GoalState::InProgress);
        assert_eq!(goal_transition(GoalState::InProgress, &Action::PublishAchieved).unwrap(), GoalState::Achieved);
        assert_eq!(goal_transition(GoalState::Achieved, &Action::CloseGoal).unwrap(), GoalState::Closed);
    }

    #[test]
    fn goal_refine_flow() {
        assert_eq!(goal_transition(GoalState::Shared, &Action::PublishRefine).unwrap(), GoalState::Refining);
        assert_eq!(goal_transition(GoalState::InProgress, &Action::PublishRefine).unwrap(), GoalState::Refining);
        assert_eq!(goal_transition(GoalState::Refining, &Action::PublishStart).unwrap(), GoalState::InProgress);
    }

    #[test]
    fn goal_reopen_from_achieved() {
        // an "achieved" candidate can be rejected by the owner and reopened
        assert_eq!(goal_transition(GoalState::Achieved, &Action::PublishStart).unwrap(), GoalState::InProgress);
        assert_eq!(goal_transition(GoalState::Achieved, &Action::PublishResumed).unwrap(), GoalState::InProgress);
        assert_eq!(goal_transition(GoalState::Achieved, &Action::PublishRefine).unwrap(), GoalState::Refining);
        // closing is still the only way to reach the terminal closed state
        assert_eq!(goal_transition(GoalState::Achieved, &Action::CloseGoal).unwrap(), GoalState::Closed);
    }

    #[test]
    fn goal_illegal_transitions_rejected() {
        // closed goals are terminal
        for a in [Action::ShareGoal, Action::PublishStart, Action::PublishProgress, Action::PublishAchieved, Action::CloseGoal] {
            assert!(goal_transition(GoalState::Closed, &a).is_err(), "{a}");
        }
        // cannot jump ahead
        assert!(goal_transition(GoalState::Proposed, &Action::PublishStart).is_err());
        assert!(goal_transition(GoalState::Shared, &Action::PublishBlocked).is_err());
        assert!(goal_transition(GoalState::Proposed, &Action::PublishAchieved).is_err());
    }

    #[test]
    fn state_string_roundtrip() {
        for s in ["forming", "active", "blocked", "completed", "archived"] {
            assert_eq!(TeamState::from_str(s).unwrap().as_str(), s);
        }
        for s in ["pending", "active", "waiting", "idle", "left", "denied"] {
            assert_eq!(MemberState::from_str(s).unwrap().as_str(), s);
        }
        for s in ["proposed", "shared", "refining", "in_progress", "blocked", "achieved", "closed"] {
            assert_eq!(GoalState::from_str(s).unwrap().as_str(), s);
        }
        assert!(TeamState::from_str("nope").is_none());
        assert!(MemberState::from_str("nope").is_none());
        assert!(GoalState::from_str("nope").is_none());
    }
}
