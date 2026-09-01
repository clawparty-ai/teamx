use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "teamx", version, about = "teamx - shared-goal team collaboration state kernel")]
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

    /// Local client config: manage local members (each may connect to a
    /// different server) and local environment settings.
    #[command(subcommand)]
    Local(LocalCmd),

    /// Session identity: list the local machine's teamx session keys
    /// (instance:session) and how to resume them
    #[command(subcommand)]
    Session(SessionCmd),

    /// Task management (built-in `taskx` doc type): assign, track and close
    /// team tasks. Tasks live as documents (content in git, state in .meta.json).
    #[command(subcommand)]
    Task(TaskCmd),

    /// opencode plugin management: install/uninstall the teamx plugin bundle
    /// into the opencode config directory (used by Homebrew and manual installs)
    #[command(subcommand)]
    Plugin(PluginCmd),

    /// PKI management (mTLS certificates)
    #[command(subcommand)]
    Cert(CertCmd),

    /// User (person) management: list users and their bound members
    #[command(subcommand)]
    User(UserCmd),

    /// Run as a network-mode server (HTTP RPC + WebSocket push + tunnels)
    Serve(ServeCmd),

    /// Reverse tunnels (network mode): expose a local service to teammates
    #[command(subcommand)]
    Tunnel(TunnelCmd),

    /// SOCKS5 outbound proxy (network mode): a local SOCKS5 port on member-a
    /// tunnels traffic through the team server to member-b's proxy exit.
    #[command(subcommand)]
    Proxy(ProxyCmd),

    /// tun0 virtual NIC (network mode, needs root): a TUN device that routes
    /// matching traffic through teamx proxy exits without configuring apps.
    #[command(subcommand)]
    Tun0(Tun0Cmd),

    /// DNS utilities (network mode): list default DNS, or resolve a domain
    /// through a proxy exit's uncensored resolver.
    #[command(subcommand)]
    Dns(DnsCmd),

    /// Git repository management (network mode): clone, pull, push via mTLS
    #[command(subcommand)]
    Git(GitCmd),

    /// Desktop tray app (L1): manage tun0 / SOCKS5 proxy from the menu bar
    /// or system tray. Needs a desktop session.
    Gui,

    /// Native control-panel window (L1): status + start/stop for tun0 and
    /// the SOCKS5 proxy, and the default exit. Spawned by `teamx gui`.
    GuiPanel,

    /// Member-side window (L1): import an invitation letter, manage reverse
    /// tunnel port mappings (expose/forward/close) and toggle the SOCKS5
    /// proxy. Cross-platform (macOS / Linux / Windows); no privileged ops.
    GuiMember,
}

/// `teamx tun0` subcommands.
#[derive(Subcommand, Debug)]
pub enum Tun0Cmd {
    /// Start the tun0 virtual NIC: create the device, inject the fake-ip
    /// route, run the fake-ip DNS and bridge traffic to the configured exits.
    /// Requires root. Long-lived.
    Start {
        /// server URL (default: TEAMX_SERVER_URL or auto-discovered letter)
        #[arg(long)]
        server: Option<String>,
        /// default exit when no route matches (optional if -f provides one)
        #[arg(long)]
        exit: Option<String>,
        /// routing table JSON file (per-target domain/IP -> exit)
        #[arg(long, short = 'f', value_name = "PATH")]
        routes: Option<PathBuf>,
        /// External rules config YAML (compat mode): maps rules
        /// (DOMAIN-SUFFIX/DOMAIN/IP-CIDR/MATCH) onto the teamx route table.
        /// Takes precedence over -f/--routes when both are given.
        #[arg(long, value_name = "PATH")]
        rules_config: Option<PathBuf>,
        /// tun interface IP (gateway for the fake-ip net)
        #[arg(long, default_value = "198.18.0.1")]
        ip: std::net::Ipv4Addr,
        /// fake-ip network (CIDR prefix)
        #[arg(long, default_value = "15")]
        net_prefix: u8,
        /// fake-ip network base
        #[arg(long, default_value = "198.18.0.0")]
        net: std::net::Ipv4Addr,
        /// max concurrent TCP connections
        #[arg(long, default_value_t = 64)]
        max_conns: usize,
        /// Enable fake-ip DNS hijacking (set system DNS to the tun gateway and
        /// answer with fake IPs). Off by default: macOS mDNSResponder does not
        /// reliably accept the 198.18.0.0/15 responses, so IP-based routing is
        /// the default transparent-proxy strategy.
        #[arg(long, default_value_t = false)]
        fake_dns: bool,
    },
    /// Stop the tun0 device (removes the route + device). Requires root.
    Stop {
        /// fake-ip network base (to remove the route)
        #[arg(long, default_value = "198.18.0.0")]
        net: std::net::Ipv4Addr,
        /// fake-ip network prefix
        #[arg(long, default_value = "15")]
        net_prefix: u8,
        /// tun interface name (default tun0)
        #[arg(long, default_value = "tun0")]
        dev: String,
    },
    /// Show tun0 status (device exists?).
    Status,
}

/// `teamx dns` subcommands.
#[derive(Subcommand, Debug)]
pub enum DnsCmd {
    /// List the default system DNS servers.
    List,
    /// Resolve a domain through a proxy exit's uncensored resolver.
    Resolve {
        /// domain to resolve
        #[arg(value_name = "DOMAIN")]
        domain: String,
        /// proxy exit name (default: the route table's default exit)
        #[arg(long)]
        exit: Option<String>,
    },
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
pub enum UserCmd {
    /// List users (persons) and the members bound to each (owner/lead only)
    List {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
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
    /// Promote a member to a backup team lead / co-lead (team lead only)
    PromoteLead {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        session: String,
        /// target team id (required only when the session belongs to several teams)
        #[arg(long)]
        team: Option<String>,
    },
    /// Remove a member's backup team lead status (team lead only)
    DemoteLead {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        session: String,
        /// target team id (required only when the session belongs to several teams)
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
        /// bind this device to a person by display name (created if absent; reuse
        /// the same name to add another device to the same user)
        #[arg(long)]
        user_name: Option<String>,
        /// bind this device to an existing person by user id (overrides --user-name)
        #[arg(long)]
        user: Option<String>,
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

/// Local client config commands (per-machine, not server-side).
#[derive(Subcommand, Debug)]
pub enum LocalCmd {
    /// List local members.
    MemberList,
    /// Add a local member (each connects to a different server).
    MemberAdd {
        /// unique key (e.g. m1)
        #[arg(value_name = "KEY")]
        key: String,
        /// display name
        #[arg(value_name = "NAME")]
        name: String,
        /// server URL this member connects to
        #[arg(long)]
        server: String,
        /// imported letter id (~/.teamx/letters/<id>) for mTLS
        #[arg(long)]
        letter: Option<String>,
        /// local proxy port
        #[arg(long, default_value_t = 1080)]
        proxy_port: u16,
        /// local fake-DNS port
        #[arg(long, default_value_t = 53)]
        dns_port: u16,
    },
    /// Update a local member's config.
    MemberUpdate {
        /// member key
        #[arg(value_name = "KEY")]
        key: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        letter: Option<String>,
        #[arg(long)]
        proxy_port: Option<u16>,
        #[arg(long)]
        dns_port: Option<u16>,
    },
    /// Remove a local member.
    MemberRemove {
        #[arg(value_name = "KEY")]
        key: String,
    },
    /// Read a local setting value.
    Get {
        #[arg(value_name = "KEY")]
        key: String,
    },
    /// Write a local setting value.
    Set {
        #[arg(value_name = "KEY")]
        key: String,
        #[arg(value_name = "VALUE")]
        value: String,
    },
}

/// opencode plugin management (used by Homebrew and manual installs).
#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Install the teamx opencode plugin bundle into the opencode config
    /// directory. The bundle is a checkout of the teamx repo (contains
    /// `opencode-plugin/dist/teamx.js` + `opencode-plugin/assets/`); when
    /// omitted, the current working directory's `opencode-plugin/` is used.
    Install {
        /// path to the teamx repo root or the opencode-plugin directory
        #[arg(value_name = "PATH")]
        path: Option<std::path::PathBuf>,
        /// override the opencode config directory (default ~/.config/opencode)
        #[arg(long)]
        config_dir: Option<std::path::PathBuf>,
        /// skip language detection; force English command files (.en.md)
        #[arg(long)]
        english: bool,
    },
    /// Remove the teamx plugin pieces from the opencode config directory.
    Uninstall {
        /// override the opencode config directory (default ~/.config/opencode)
        #[arg(long)]
        config_dir: Option<std::path::PathBuf>,
    },
}

/// Session identity commands: find and resume this machine's teamx sessions.
#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    /// List every teamx member session known on this machine: the full
    /// `instance:session` key, the member/team/role it maps to, and how to
    /// resume it (opencode session or certificate-bound network identity).
    List {
        /// include only members whose session belongs to this instance
        /// (default: all local members)
        #[arg(long)]
        this_instance: bool,
    },
}

/// Task commands — teamx's built-in `taskx` document type.
///
/// Tasks are documents: the content lives in git (`taskx/<id>.md`), the state
/// machine in `.teamx/docs/taskx/<id>.meta.json`, and every transition is an
/// auditable ledger event. `task create`/`task done`/etc. map onto the doc
/// engine (`doc.created` / `doc.done` / ...).
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub enum TaskCmd {
    /// Create a task (team lead). Writes taskx/<id>.md + .meta.json, broadcasts
    /// `doc.created` and auto git-commits unless --no-push.
    Create {
        /// task title (also used as the doc id slug)
        #[arg(value_name = "TITLE")]
        title: String,
        /// assignee member id (a specific member)
        #[arg(long)]
        assignee: Option<String>,
        /// assignee role key (assign to every active member of a role)
        #[arg(long)]
        role: Option<String>,
        /// executor kind: agent (default) or human
        #[arg(long, default_value = "agent", value_parser = ["agent", "human"])]
        executor: String,
        /// priority: high / medium / low
        #[arg(long, default_value = "medium", value_parser = ["high", "medium", "low"])]
        priority: String,
        /// task id (default: auto `task-<n>`)
        #[arg(long)]
        id: Option<String>,
        /// detail / background for the task body
        #[arg(long)]
        detail: Option<String>,
        /// do not auto git commit+push
        #[arg(long)]
        no_push: bool,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Acknowledge receipt of a task (auto-issued by the plugin on assignee
    /// sessions; may also be called manually).
    Ack {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Claim a task (optional; small tasks may skip straight to work).
    Claim {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Record progress on a task (doc.updated).
    Update {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        /// progress note
        #[arg(long)]
        progress: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Request help from the team lead (notification event; task stays in_progress).
    Help {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        /// what you need help with
        #[arg(long)]
        reason: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Mark a task done (completion candidate; the lead verifies).
    Done {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        /// result / deliverable summary
        #[arg(long)]
        result: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Verify a completed task (team lead) — closes the loop.
    Verify {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Reject a task back to work (team lead), with a reason.
    Reject {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List tasks (all, or filtered). `--mine` shows tasks assigned to the
    /// current session's member.
    List {
        /// only tasks assigned to the current member
        #[arg(long)]
        mine: bool,
        /// filter by state (assigned/acked/claimed/in_progress/done/verified)
        #[arg(long)]
        state: Option<String>,
        /// filter by assignee member id
        #[arg(long)]
        assignee: Option<String>,
        /// filter by executor kind (agent/human)
        #[arg(long)]
        executor: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        team: Option<String>,
    },
    /// Show a task's full audit history (ledger events + meta transitions).
    Log {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        team: Option<String>,
    },
}

/// Reverse-tunnel commands (network mode).
///
/// These manage the server's tunnel registry. `expose` is issued by the
/// provider (member opening a tunnel); `list`/`status`/`close` are read/control
/// operations that any member can run against the server.
#[derive(Subcommand, Debug)]
pub enum TunnelCmd {
    /// Expose a local service to teammates through the server (provider side).
    /// Opens a persistent WebSocket to the server and registers a tunnel.
    Expose {
        /// public tunnel name (unique per team)
        #[arg(value_name = "NAME")]
        name: String,
        /// local port to expose
        #[arg(long)]
        port: u16,
        /// provider LAN IP for direct-connect hints (auto-detected if absent)
        #[arg(long)]
        lan_ip: Option<String>,
        /// exposure mode: local (default, server binds no port; consumers use
        /// `tunnel forward`) or frp (server binds a public port)
        #[arg(long, default_value = "local")]
        mode: String,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List exposed tunnels of the current team (server registry).
    List {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Show one tunnel's status, including a same-subnet direct-connect hint.
    Status {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Close an exposed tunnel (frees its public port).
    Close {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Forward a teammate's tunnel to a local port (consumer, local-forward mode).
    Forward {
        #[arg(value_name = "NAME")]
        name: String,
        /// local port to listen on (default: provider's target port)
        #[arg(long)]
        local_port: Option<u16>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
}

/// SOCKS5 outbound proxy commands (network mode).
///
/// - `proxy exit`: provider side — member-b registers a proxy exit and dials
///   the target of every incoming SOCKS5 stream (long-lived).
/// - `proxy start`: consumer side — member-a serves a local SOCKS5 port and
///   tunnels every CONNECT through the team server to the exit (long-lived).
#[derive(Subcommand, Debug)]
pub enum ProxyCmd {
    /// Start a proxy exit (provider side): register with the server and dial
    /// the SOCKS5 target of each stream. Long-lived.
    Exit {
        /// exit name (unique per team; consumed via `proxy start --exit`)
        #[arg(value_name = "NAME")]
        name: String,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Serve a local SOCKS5 proxy port that tunnels to a team proxy exit.
    /// Long-lived.
    Start {
        /// local SOCKS5 port to listen on
        #[arg(long, default_value_t = 1080)]
        port: u16,
        /// proxy exit name to route traffic through (default; used as the
        /// fixed exit unless -f/--routes or the SQLite table provides one)
        #[arg(long)]
        exit: Option<String>,
        /// routing table JSON file (per-target domain/IP -> exit). Overrides
        /// the SQLite route table for this invocation. `-f` is the short
        /// alias. See docs/08-design-proxy-routes.md.
        #[arg(long, short = 'f', value_name = "PATH")]
        routes: Option<PathBuf>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Manage the SQLite-backed proxy routing table (per-target domain/IP ->
    /// exit) used by `proxy start` when no -f/--routes file is given.
    #[command(subcommand)]
    Routes(RoutesCmd),
}

/// Subcommands for the SQLite-backed proxy routing table.
#[derive(Subcommand, Debug)]
pub enum RoutesCmd {
    /// Show the current routing table (default exit + rules).
    List,
    /// Add or update a rule: <match> <exit>. Appends unless `--seq` given.
    Add {
        /// match pattern: `*.cn`, `example.com`, `10.0.0.0/8`, `192.168.1.5`
        #[arg(value_name = "MATCH")]
        match_: String,
        /// exit name to route matched targets through
        #[arg(value_name = "EXIT")]
        exit: String,
        /// position in the rule list (first-match order); default append
        #[arg(long)]
        seq: Option<i64>,
    },
    /// Remove a rule by its match pattern.
    Remove {
        /// the exact match pattern to remove
        #[arg(value_name = "MATCH")]
        match_: String,
    },
    /// Set the default exit used when no rule matches.
    SetDefault {
        /// exit name
        #[arg(value_name = "EXIT")]
        exit: String,
    },
    /// Import a route table from a JSON file (replaces the whole table).
    Import {
        /// JSON route table file
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Clear all rules (keeps the default exit).
    Clear,
}

/// Git repository commands (network mode).
///
/// These manage git repositories on the teamx server. Members can clone,
/// pull, and push repositories through the mTLS-secured connection.
#[derive(Subcommand, Debug)]
pub enum GitCmd {
    /// Configure stock `git` to talk to the teamx server over mTLS: writes
    /// http.<server>/sslCert/Key/CAInfo from the invitation letter into the
    /// local git config, so plain `git clone/pull/push` works.
    Setup {
        /// server URL (default: TEAMX_SERVER_URL or discovered letter)
        #[arg(long)]
        server: Option<String>,
        /// write git config to the current repo instead of ~/.gitconfig
        #[arg(long)]
        local: bool,
    },
    /// Clone a repository from the server
    Clone {
        /// repository name
        #[arg(value_name = "REPO")]
        repo: String,
        /// local directory to clone into (default: repo name)
        #[arg(long)]
        directory: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Pull (fetch + merge) from the remote repository
    Pull {
        /// repository name
        #[arg(value_name = "REPO")]
        repo: String,
        /// branch to pull (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// working directory (default: current dir)
        #[arg(long)]
        dir: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Push changes to the remote repository
    Push {
        /// repository name
        #[arg(value_name = "REPO")]
        repo: String,
        /// branch to push (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// working directory (default: current dir)
        #[arg(long)]
        dir: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Commit local changes (git add -A + commit)
    Commit {
        /// commit message
        #[arg(long, short = 'm')]
        message: String,
        /// working directory (default: current dir)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Commit local changes then push to the remote repository
    CommitPush {
        /// commit message
        #[arg(long, short = 'm')]
        message: String,
        /// repository name (default: the cloned repo)
        #[arg(long)]
        repo: Option<String>,
        /// branch to push (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// working directory (default: current dir)
        #[arg(long)]
        dir: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List repositories accessible to the current member
    List {
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Create a new repository (owner/admin only)
    Create {
        /// repository name
        #[arg(value_name = "NAME")]
        name: String,
        /// description (optional)
        #[arg(long)]
        description: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Delete a repository (owner/admin only)
    Delete {
        /// repository name
        #[arg(value_name = "NAME")]
        name: String,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Grant a member access to a repository (owner/admin only)
    Grant {
        /// repository name
        #[arg(value_name = "NAME")]
        name: String,
        /// member id to grant access to
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        /// permission level: read, write, admin (default read)
        #[arg(long, default_value = "read")]
        permission: String,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Show access permissions of a repository (owner/admin only)
    Permissions {
        /// repository name
        #[arg(value_name = "NAME")]
        name: String,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
}
