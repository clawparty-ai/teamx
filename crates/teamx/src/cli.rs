use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "teamx", version, about = "teamx - team collaboration state kernel (V1, local single-machine)")]
pub struct Cli {
    /// SQLite database path (default: ~/.teamx/teamx.db, or $TEAMX_DB)
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,

    /// Emit machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize the global database
    Init,

    /// Team management
    #[command(subcommand)]
    Team(TeamCmd),

    /// Goal management
    #[command(subcommand)]
    Goal(GoalCmd),

    /// Member management
    #[command(subcommand)]
    Member(MemberCmd),

    /// Role catalog
    #[command(subcommand)]
    Role(RoleCmd),

    /// Publish a generic event (progress/decision/update/blocked/resumed/achieved/refine/start/activity)
    Publish {
        /// event type
        #[arg(value_name = "TYPE")]
        r#type: String,
        /// JSON payload for the event
        #[arg(long)]
        data: Option<String>,
        /// assign the task/event to a specific member (auto-execute on that member only)
        #[arg(long)]
        assignee: Option<String>,
        /// actor session key
        #[arg(long)]
        session: String,
        /// target team id (required only when the session belongs to several teams)
        #[arg(long)]
        team: Option<String>,
    },

    /// Ask a question to a member (marks the member waiting)
    Ask {
        /// target member id
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        question: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },

    /// Answer an open question
    Respond {
        /// question id
        #[arg(value_name = "ASK_ID")]
        ask_id: String,
        #[arg(long)]
        answer: String,
        #[arg(long)]
        session: String,
    },

    /// List raw ledger events for a team
    Events {
        /// only events with seq greater than this
        #[arg(long)]
        after: Option<i64>,
        #[arg(long)]
        team: Option<String>,
    },

    /// Human-readable audit replay of a team's event timeline
    Log {
        #[arg(long)]
        team: Option<String>,
        /// resolve the team from this session (single-team sessions only)
        #[arg(long)]
        session: Option<String>,
        /// show only the last N events
        #[arg(long)]
        limit: Option<i64>,
        /// only events with seq greater than this
        #[arg(long)]
        after: Option<i64>,
    },

    /// Pull the latest team state + incremental events and advance the session cursor
    Sync {
        /// actor session key
        #[arg(long)]
        session: String,
        /// do not advance the sync cursor
        #[arg(long)]
        no_advance: bool,
    },

    /// loopx bridge
    #[command(subcommand)]
    Loopx(LoopxCmd),

    /// PKI management (mTLS certificates)
    #[command(subcommand)]
    Cert(CertCmd),

    /// Run as a network-mode server (HTTP RPC + WebSocket push)
    Serve(ServeCmd),
}

#[derive(Args, Debug, Clone)]
pub struct ServeCmd {
    /// bind address
    #[arg(long, default_value = "127.0.0.1")]
    pub addr: String,
    /// bind port
    #[arg(long, default_value_t = 5781)]
    pub port: u16,
    /// SQLite database path (default: ~/.teamx/teamx.db, or $TEAMX_DB)
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// extra SAN (e.g. LAN IP) to include in the server certificate
    #[arg(long)]
    pub san: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum CertCmd {
    /// Ensure the instance CA + server cert exist (create if absent)
    Init,
    /// Issue a member client certificate (signed by the instance CA)
    Issue {
        /// member id (goes into the certificate CN)
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        /// role key (goes into the certificate CN)
        #[arg(value_name = "ROLE")]
        role: String,
        /// write the certificate + key to these files
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print the CA certificate (for client-side trust)
    Ca,
}

#[derive(Subcommand, Debug)]
pub enum TeamCmd {
    /// Create a team (the creating session becomes owner)
    Create {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long)]
        session: String,
        /// optional goal title drafted at creation
        #[arg(long)]
        goal_title: Option<String>,
        #[arg(long)]
        goal_body: Option<String>,
    },
    /// Join a team via invite token (creates a pending membership)
    Join {
        #[arg(value_name = "TOKEN")]
        token: String,
        /// display name chosen by the user at join time
        #[arg(long)]
        name: String,
        #[arg(long)]
        session: String,
        /// optional loopx project directory for the stage-progress bridge
        #[arg(long)]
        loopx_project: Option<PathBuf>,
    },
    /// Approve a pending membership (owner only)
    Approve {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        session: String,
        /// target team id (required only when the owner session belongs to several teams)
        #[arg(long)]
        team: Option<String>,
    },
    /// Deny a pending membership (owner only)
    Deny {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        session: String,
        /// target team id (required only when the owner session belongs to several teams)
        #[arg(long)]
        team: Option<String>,
    },
    /// List teams the current session belongs to
    List {
        #[arg(long)]
        session: String,
    },
    /// Show team status (owner view of the whole team)
    Status {
        #[arg(long)]
        team: Option<String>,
        /// session key used to resolve the team when --team is absent
        #[arg(long)]
        session: Option<String>,
    },
    /// Leave a team
    Leave {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Archive a completed team (owner only)
    Archive {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Soft-destroy a team (owner only): hide it from lists, revoke invitations, keep data
    Destroy {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Invite a member with a job role: issue a client cert + invitation letter (owner only)
    Invite {
        /// job role + description, e.g. "测试工程师: 负责测试并汇报缺陷"
        #[arg(value_name = "ROLE_DESC")]
        role_desc: String,
        /// suggested display name (hint only; the member may override at import)
        #[arg(long)]
        name_hint: Option<String>,
        /// server URL to embed in the letter (default https://127.0.0.1:5781)
        #[arg(long)]
        server_url: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List issued (unused/revoked) invitation letters (owner only)
    InviteList {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Revoke an invitation letter (its cert is rejected at connect) (owner only)
    InviteRevoke {
        #[arg(value_name = "INVITATION_ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Import an invitation letter: store the client cert/key and claim the pending seat
    Import {
        /// invitation letter (single-line `teamx-inv:v1:<base64>` or a path to a .json letter)
        #[arg(value_name = "LETTER")]
        letter: String,
        /// display name (defaults to the letter's name_hint)
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        session: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum GoalCmd {
    /// Set (or update) the team goal (owner only)
    Set {
        #[arg(value_name = "TITLE")]
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Share the goal with members (owner only)
    Share {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Verify and close the goal; team becomes completed (owner only)
    Close {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemberCmd {
    /// Set a member's working state: idle (finished current slice) or active (resumed)
    SetState {
        /// target state
        #[arg(value_name = "STATE", value_parser = ["idle", "active"])]
        state: String,
        /// target member id (owner only); defaults to the acting session
        #[arg(long)]
        member: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum RoleCmd {
    /// List the role catalog
    List {
        #[arg(long)]
        team: Option<String>,
    },
    /// Set the current session's role (member self-service; owner may specify on their behalf)
    Set {
        #[arg(value_name = "ROLE")]
        role: String,
        #[arg(long)]
        session: String,
        /// when set, applies the role to a different member (owner only)
        #[arg(long)]
        member: Option<String>,
        #[arg(long)]
        team: Option<String>,
    },
    /// Propose a custom role (member self-service); the owner must approve it before it can be used
    Propose {
        #[arg(value_name = "KEY")]
        role: String,
        #[arg(value_name = "LABEL")]
        label: String,
        #[arg(value_name = "DESCRIPTION")]
        description: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Approve a proposed custom role and grant it to the proposer (owner only)
    Approve {
        #[arg(value_name = "KEY")]
        role: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Deny a proposed custom role and remove the proposal (owner only)
    Deny {
        #[arg(value_name = "KEY")]
        role: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Update a role's label/description (owner only)
    Update {
        #[arg(value_name = "KEY")]
        role: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoopxCmd {
    /// Snapshot loopx stage progress for a bound project and publish it as a loopx.progress event
    Report {
        #[arg(value_name = "PROJECT")]
        project: PathBuf,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
}
