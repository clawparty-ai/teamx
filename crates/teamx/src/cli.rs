use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "teamx",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (commit ",
        env!("TEAMX_BUILD_COMMIT"),
        ", built ",
        env!("TEAMX_BUILD_TIME"),
        ")"
    ),
    about = "teamx - shared-goal team collaboration state kernel"
)]
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

    /// Print build info (version / git commit / build time)
    Version,

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

    /// Group discussion between team members (threads stored as ledger events)
    #[command(subcommand)]
    Discussion(DiscussionCmd),

    /// IM access: identity binding (one-time codes) for remote operation
    #[command(subcommand)]
    Im(ImCmd),

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

    /// Cancel an open question (owner/co-lead only). Unlocks the target member
    /// (waiting → active) when they have no other open question.
    AskCancel {
        /// question id
        #[arg(value_name = "ASK_ID")]
        ask_id: String,
        /// optional reason (recorded in the ledger)
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },

    /// Customize nudge message templates (owner/co-lead only; Issue #15).
    Nudge {
        #[command(subcommand)]
        action: NudgeAction,
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

    /// Gateway process B: forward a delegated IM action via a service cert
    #[command(subcommand)]
    ImGateway(ImGatewayCmd),

    /// PKI management (mTLS certificates)
    #[command(subcommand)]
    Cert(CertCmd),

    /// User (person) management: list users and their bound members
    #[command(subcommand)]
    User(UserCmd),

    /// Team definition file (.teamx/TEAM.md) utilities
    #[command(subcommand)]
    Teamfile(TeamfileCmd),

    /// Run as a network-mode server (HTTP RPC + WebSocket push + tunnels).
    ///
    /// Env: TEAMX_MAX_BODY overrides the /rpc request body limit (bytes,
    /// default 512 MiB — large enough for base64 git bundles). Read once at
    /// startup; changing it requires a restart.
    Serve(ServeCmd),

    /// Reverse tunnels (network mode): expose a local service to teammates
    #[command(subcommand)]
    Tunnel(TunnelCmd),

    /// Member-to-member direct connectivity (data-plane rendezvous): advertise
    /// candidates and probe a peer for a direct path (control stays on server).
    #[command(subcommand)]
    Direct(DirectCmd),

    /// Member activity analytics (enterprise): push/query activity rows
    #[command(subcommand)]
    Activity(ActivityCmd),

    /// Model cost pricing (enterprise): set/list per-model token prices (USD/1M)
    #[command(subcommand)]
    Model(ModelCmd),

    /// Web dashboard (enterprise): activity analytics UI (HTTPS + token, owner only)
    Ui(UiCmd),

    /// Member web console (local, view-only): a workspace-scoped browser
    /// console that shows the member's teams/tasks/events/tunnels and embeds
    /// cloud opencode nodes. Reads the same data the teamx CLI sees.
    Console(ConsoleCmd),

    /// Always-on node worker (autonomous): polls the unified inbox and runs
    /// headless `opencode run` turns so a member node progresses tasks even
    /// without an attached TUI. Dry-run unless TEAMX_WORKER_ENABLED=1.
    Worker(WorkerCmd),

    /// Headless agent (opencode instance) manager: spawn/list/stop multiple
    /// `opencode serve` processes (one workspace per agent) so one machine can
    /// run many AI agent members without an external orchestrator.
    #[command(subcommand)]
    Agents(AgentsCmd),

    /// One-command local single-machine multi-agent launch from .teamx/TEAM.md
    #[command(subcommand)]
    LocalTeam(LocalTeamCmd),

    /// Member node file service (workspace browser backend). Binds 127.0.0.1
    /// and exposes list/read/download/upload under the startup directory.
    Fs(FsCmd),

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

    /// Traffic capture (enterprise): record flows/streams into capture.db.
    #[command(subcommand)]
    Capture(CaptureCmd),

    /// Traffic analysis (enterprise): stats, HTTP reconstruction, TLS decrypt.
    #[command(subcommand)]
    Analyze(AnalyzeCmd),

    /// Traffic replay (enterprise): TCP client/mock replay + HTTP exchange.
    #[command(subcommand)]
    Replay(ReplayCmd),

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

/// Sub-actions for `teamx nudge` (Issue #15).
#[derive(Debug, clap::Subcommand)]
pub enum NudgeAction {
    /// Set a custom template for a nudge kind (owner/co-lead only).
    Set {
        /// nudge kind (idle_reminder / lead_reminder / claim_reminder / task_nudge)
        kind: String,
        /// template text with {placeholders}; max 2000 chars
        #[arg(long)]
        template: Option<String>,
        /// read the template from a file instead of --template
        #[arg(long)]
        template_file: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Show the configured template for a nudge kind (null when unset).
    Get {
        kind: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Remove the custom template (falls back to the built-in default).
    Reset {
        kind: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
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
        /// Record flows/streams into capture.db.
        #[arg(long)]
        capture: bool,
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

#[derive(Subcommand, Debug)]
pub enum ActivityCmd {
    /// Push a batch of activity rows into the local database (V1 mode)
    Push {
        /// JSON array of activity rows (ActivityRow[]), each with team_id,
        /// member_id, node_id, node_name, started_at, kind, detail, tokens, cost...
        #[arg(long)]
        data: String,
        /// actor session key (resolves member_id in V1 local mode)
        #[arg(long)]
        session: String,
    },
    /// Query activity from the local database (V1 mode)
    #[command(subcommand)]
    Query(ActivityQueryCmd),
}

#[derive(Subcommand, Debug)]
pub enum ActivityQueryCmd {
    /// Aggregate overview for a team
    Summary {
        #[arg(long)]
        team: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
    },
    /// Recent activity rows
    Rows {
        #[arg(long)]
        team: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        member: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
}

/// `teamx model` subcommands (enterprise cost pricing).
#[derive(Subcommand, Debug)]
pub enum ModelCmd {
    /// Set (or override) a model's token price. `<model>` is `<provider>/<model>`.
    Set {
        /// provider/model id, e.g. `anthropic/claude-sonnet-4-5`
        #[arg(value_name = "PROVIDER/MODEL")]
        model: String,
        /// USD per 1M input tokens
        #[arg(long)]
        input: f64,
        /// USD per 1M output tokens
        #[arg(long)]
        output: f64,
        /// USD per 1M cache-read tokens (default 0)
        #[arg(long, default_value_t = 0.0)]
        cache_read: f64,
        /// USD per 1M cache-write tokens (default 0)
        #[arg(long, default_value_t = 0.0)]
        cache_write: f64,
        /// >200k context tier: USD per 1M input tokens (optional)
        #[arg(long)]
        over200k_input: Option<f64>,
        /// >200k context tier: USD per 1M output tokens (optional)
        #[arg(long)]
        over200k_output: Option<f64>,
        /// >200k context tier: USD per 1M cache-read tokens (optional)
        #[arg(long)]
        over200k_cache_read: Option<f64>,
        /// >200k context tier: USD per 1M cache-write tokens (optional)
        #[arg(long)]
        over200k_cache_write: Option<f64>,
    },
    /// List all model prices.
    Ls,
}

#[derive(Args, Debug, Clone)]
pub struct UiCmd {
    /// bind address
    #[arg(long, default_value = "127.0.0.1")]
    pub addr: String,
    /// bind port
    #[arg(long, default_value_t = 9527)]
    pub port: u16,
    /// actor session key (the team lead's session, verified as an owner)
    #[arg(long)]
    pub session: String,
    /// SQLite database path (default: ~/.teamx/teamx.db, or $TEAMX_DB)
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// teamx server URL (default: TEAMX_SERVER_URL or discovered letter) —
    /// required to reach member tunnels (live sessions / web terminal).
    #[arg(long)]
    pub server_url: Option<String>,
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

/// `teamx console` — member web console (local HTTPS + token, view-only).
///
/// Runs in a workspace: its data source is whatever `teamx` sees from that
/// directory (project-scoped `.teamx` DB in project mode, or the teamx server
/// over mTLS in network mode). The served pages reuse the owner dashboard
/// (`teamx ui`) look & feel but are branded "Teamx Console".
#[derive(Args, Debug, Clone)]
pub struct ConsoleCmd {
    /// bind address
    #[arg(long, default_value = "127.0.0.1")]
    pub addr: String,
    /// bind port
    #[arg(long, default_value_t = 9528)]
    pub port: u16,
    /// optional actor session key (project mode). When absent the console
    /// picks the workspace's single local member session, or requires it.
    #[arg(long)]
    pub session: Option<String>,
    /// cloud-node list file (default: <teamx_home>/console.json)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// teamx server URL override (default: TEAMX_SERVER_URL, else the URL in
    /// the workspace's imported invitation letter)
    #[arg(long)]
    pub server_url: Option<String>,
}


/// `teamx fs` — member node file service.
#[derive(Args, Debug, Clone)]
pub struct FsCmd {
    /// bind address
    #[arg(long, default_value = "127.0.0.1")]
    pub addr: String,
    /// bind port
    #[arg(long, default_value_t = 4197)]
    pub port: u16,
}

/// `teamx worker` — autonomous node executor.
#[derive(Args, Debug, Clone)]
pub struct WorkerCmd {
    /// poll interval in seconds
    #[arg(long, default_value_t = 60)]
    pub interval: u64,
    /// minimum seconds between actual opencode turns (nudge throttle)
    #[arg(long, default_value_t = 600)]
    pub cooldown: u64,
    /// only poll once (useful for dry-run / debugging)
    #[arg(long)]
    pub once: bool,
    /// teamx server URL (default: TEAMX_SERVER_URL or letter)
    #[arg(long)]
    pub server_url: Option<String>,
    /// opencode binary to run headless turns (default: opencode)
    #[arg(long)]
    pub opencode: Option<String>,
    /// the node's resident opencode serve URL (required: no default —
    /// a silent 4199 default attached to the wrong workspace on
    /// multi-workspace nodes; falls back to TEAMX_OPENCODE_URL)
    #[arg(long)]
    pub opencode_url: Option<String>,
}

/// `teamx agents` — manage headless opencode agent instances (multi-agent runs).
#[derive(Subcommand, Debug, Clone)]
pub enum AgentsCmd {
    /// Spawn a headless `opencode serve` instance (one workspace per agent).
    Spawn {
        /// instance name (unique per machine; also used as process/label)
        #[arg(long)]
        name: String,
        /// workspace / working directory for this instance (default: ./<name>)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// bind port for `opencode serve` (default: auto-pick 4200+ free port)
        #[arg(long)]
        port: Option<u16>,
        /// opencode binary path (default: `opencode` from PATH)
        #[arg(long)]
        opencode: Option<String>,
        /// extra environment as KEY=VALUE (repeatable), e.g. TEAMX_SERVER_URL
        #[arg(long = "env")]
        env: Vec<String>,
        /// run in the foreground (inherit stdio; default: daemonize + log file)
        #[arg(long)]
        foreground: bool,
    },
    /// List headless agent instances spawned on this machine.
    List {
        /// include only running instances
        #[arg(long)]
        running: bool,
    },
    /// Stop a headless agent instance (by name). SIGTERM then SIGKILL if needed.
    Stop {
        /// instance name
        #[arg(long)]
        name: String,
        /// also remove the instance registration after stopping
        #[arg(long)]
        remove: bool,
    },
}

/// `teamx local-team` — one-command launch of every member in `.teamx/TEAM.md`
/// as a local, single-machine multi-agent working set (headless opencode +
/// optional web TUI + persona + optional repo clone).
#[derive(Subcommand, Debug, Clone)]
pub enum LocalTeamCmd {
    /// Launch all non-owner members (or `--only`) from the team file.
    Up {
        /// path to TEAM.md (default <cwd>/.teamx/TEAM.md)
        #[arg(long)]
        team_file: Option<PathBuf>,
        /// shared git repo name to clone into each member dir (optional)
        #[arg(long)]
        repo: Option<String>,
        /// server URL for git clone (default TEAMX_SERVER_URL or letter)
        #[arg(long)]
        server: Option<String>,
        /// only launch these member keys (comma separated; may include owner)
        #[arg(long)]
        only: Option<String>,
        /// headless serve base port (default 4200; auto-advances)
        #[arg(long)]
        port_base: Option<u16>,
        /// ttyd base port (default 7680; auto-advances)
        #[arg(long)]
        ttyd_port_base: Option<u16>,
        /// do not launch ttyd web TUIs
        #[arg(long)]
        no_ttyd: bool,
        /// do not attempt repo clone
        #[arg(long)]
        no_git: bool,
        /// do not write AGENTS.md persona files
        #[arg(long)]
        no_persona: bool,
        /// opencode binary path (default `opencode` from PATH)
        #[arg(long)]
        opencode: Option<String>,
        /// extra environment KEY=VALUE (repeatable), passed to each headless
        #[arg(long = "env")]
        env: Vec<String>,
    },
    /// List local-team members (dir, headless port, ttyd url, pid, status).
    Ps,
    /// Stop one or all local-team members.
    Stop {
        /// member key to stop (default: all)
        #[arg(long)]
        name: Option<String>,
    },
    /// Show a member's headless/ttyd logs.
    Logs {
        /// member key
        #[arg(long)]
        name: String,
        /// number of tail lines
        #[arg(long)]
        lines: Option<u16>,
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

/// `teamx im-gateway <subcommand>` — the gateway process (identity B).
#[derive(Debug, Subcommand)]
pub enum ImGatewayCmd {
    /// End-to-end round trip: service cert mTLS → whitelist → delegation
    /// verify → binding resolve → whitelisted read (im.query).
    Check {
        /// directory holding service.crt + service.key + ca.crt
        #[arg(long)]
        cert_dir: std::path::PathBuf,
        /// server base URL (https://host:port)
        #[arg(long)]
        server: String,
        /// team scope (optional when the instance has exactly one team)
        #[arg(long)]
        team: Option<String>,
        /// provider platform (default feishu)
        #[arg(long, default_value = "feishu")]
        platform: String,
        /// delegated IM user (open_id)
        #[arg(long, value_name = "IM_USER")]
        im_user: String,
        /// im.query kind (status/goal/tasks/discussions/questions)
        #[arg(long, default_value = "status")]
        kind: String,
    },
    /// Run the persistent-connection gateway: dial Feishu's long-connection
    /// event stream, normalize inbound events and forward delegated actions to
    /// `serve` over mTLS. Long-lived (no public callback URL needed).
    Run {
        /// directory holding service.crt + service.key + ca.crt
        #[arg(long)]
        cert_dir: std::path::PathBuf,
        /// server base URL (https://host:port)
        #[arg(long)]
        server: String,
        /// team scope the gateway serves
        #[arg(long)]
        team: String,
        /// provider platform (default feishu)
        #[arg(long, default_value = "feishu")]
        platform: String,
        /// feishu app id (defaults to TEAMX_IM_FEISHU_APP_ID)
        #[arg(long)]
        app_id: Option<String>,
        /// feishu app secret (defaults to TEAMX_IM_FEISHU_APP_SECRET)
        #[arg(long)]
        app_secret: Option<String>,
        /// co-located `opencode serve` base URL (e.g. http://127.0.0.1:42630).
        /// When set, free-text messages are answered by the member's agent;
        /// fixed commands keep the fast path.
        #[arg(long)]
        agent: Option<String>,
        /// where the agent session id is persisted
        /// (defaults to <teamx_home>/im-agent-session.json)
        #[arg(long)]
        session_file: Option<std::path::PathBuf>,
    },
}

/// PKI management: CA/server/member/service certificate operations.
#[derive(Debug, Subcommand)]
pub enum CertCmd {
    /// Ensure the instance CA + server cert exist (create if absent)
    Init,
    /// Issue a service certificate for a gateway process
    /// (CN `service:<kind>:<instance>`, short TTL — design §5.3 identity B)
    IssueService {
        /// service kind (goes into the certificate CN, e.g. im-gateway)
        #[arg(value_name = "KIND")]
        kind: String,
        /// instance name (goes into the certificate CN)
        #[arg(value_name = "INSTANCE")]
        instance: String,
        /// write the certificate + key to this directory
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
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
pub enum TeamfileCmd {
    /// Validate a `.teamx/TEAM.md` team definition (used by `/team start`
    /// before saving; exits non-zero with the issues listed when invalid).
    Validate {
        /// path to TEAM.md (default: .teamx/TEAM.md)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Generate/update `.teamx/docs/_spec/<key>.json` document-contract snapshots
    /// from TEAM.md (the same snapshots `team create` writes at bootstrap).
    Sync {
        /// path to TEAM.md (default: .teamx/TEAM.md)
        #[arg(long)]
        path: Option<PathBuf>,
        /// docs dir to write (default: <TEAM.md dir>/docs/_spec)
        #[arg(long)]
        docs_dir: Option<PathBuf>,
        /// show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
        /// also remove _spec files whose doc key is no longer declared in TEAM.md
        #[arg(long)]
        prune: bool,
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
        /// auto-approve this member on import (skip the owner approval step)
        #[arg(long)]
        auto_approve: bool,
        /// grant team-lead (co-lead) status on import (primary owner only)
        #[arg(long)]
        as_lead: bool,
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
    /// Clone a team: copy members, roles, and create empty repos under a new name
    Clone {
        /// new team name (must be unique across all teams)
        #[arg(value_name = "NAME")]
        name: String,
        /// source team id (optional when the session belongs to one team)
        #[arg(long)]
        team: Option<String>,
        /// server URL to embed in each invitation letter
        #[arg(long)]
        server_url: Option<String>,
        /// skip the preview confirmation and execute immediately
        #[arg(long)]
        yes: bool,
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
    /// Get/set a member's per-topic discussion participation quota.
    DiscussionQuota {
        /// action: get or set
        #[arg(value_name = "ACTION", value_parser = ["get", "set"])]
        action: String,
        /// new quota value (required for set)
        #[arg(long)]
        value: Option<i64>,
        /// target member id (defaults to the acting session)
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

/// Team discussion subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum DiscussionCmd {
    /// Create a discussion thread (recorded as a `discussion.created` event)
    Create {
        /// thread title
        #[arg(value_name = "TITLE")]
        title: String,
        /// optional body / opening message
        #[arg(long)]
        body: Option<String>,
        /// optional associated task id
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List active discussion threads for the team
    List {
        /// include closed threads too
        #[arg(long)]
        all: bool,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Show one discussion thread (its messages, newest first)
    Open {
        /// discussion id (as returned by create)
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Post a message to a discussion
    Say {
        /// discussion id
        #[arg(value_name = "ID")]
        id: String,
        /// message body
        #[arg(value_name = "BODY")]
        body: String,
        /// JSON array of quote objects: [{"kind":"message"|"event"|"task","message_id"|"seq"|"task_id":...,"text":"..."}]
        #[arg(long)]
        quotes: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Reply to a specific message inside a discussion (sets reply_to)
    Reply {
        /// discussion id
        #[arg(value_name = "ID")]
        id: String,
        /// message id being replied to (e.g. the seq of a discussion.message event)
        #[arg(value_name = "MESSAGE_ID")]
        message_id: String,
        /// reply body
        #[arg(value_name = "BODY")]
        body: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Close a discussion (owner/lead; appends a `discussion.closed` event)
    Close {
        /// discussion id
        #[arg(value_name = "ID")]
        id: String,
        /// optional closing summary (markdown) shown under the topic; auto-generated digest if omitted
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Subscribe to a discussion so its new messages wake you (AI inbox)
    Subscribe {
        /// discussion id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Unsubscribe from a discussion
    Unsubscribe {
        /// discussion id
        #[arg(value_name = "ID")]
        id: String,
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

/// IM identity binding (P1). Codes are single-use, 5-minute TTL, and are
/// pre-associated with the member who created them, so binding needs no lead
/// approval. `bind` is the in-process gateway path (P1, design §5.3 option A):
/// the gateway calls it with the IM identity carried by the inbound message.
#[derive(Subcommand, Debug)]
pub enum ImCmd {
    /// Create a one-time binding code for yourself (send it to the team bot)
    BindCode {
        /// code lifetime in seconds (default 300, allowed 60..=900)
        #[arg(long)]
        ttl_secs: Option<i64>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Redeem a binding code as an IM identity (in-process gateway path)
    Bind {
        /// the one-time code
        #[arg(value_name = "CODE")]
        code: String,
        /// IM-side user id (feishu open_id / user_id)
        #[arg(long)]
        im_user: String,
        /// IM platform key
        #[arg(long, default_value = "feishu")]
        platform: String,
        /// IM-side tenant / team id (optional)
        #[arg(long)]
        im_team: Option<String>,
        /// provider signature over the delegated fields (optional in P1)
        #[arg(long)]
        signature: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Unbind an IM identity (your own; a lead may unbind anyone in the team)
    Unbind {
        /// IM-side user id
        #[arg(long)]
        im_user: String,
        #[arg(long, default_value = "feishu")]
        platform: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// List IM bindings (your own, or the team's with --all as a lead)
    Bindings {
        /// list every binding of the team (owner / co-lead only)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Resolve the member behind an IM identity (owner / co-lead only)
    Resolve {
        #[arg(long)]
        im_user: String,
        #[arg(long, default_value = "feishu")]
        platform: String,
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
        /// assignee role key (assign to a role; with --mode bid (default for
        /// --role) role members compete to claim; --mode broadcast gives every
        /// role member their own instance)
        #[arg(long)]
        role: Option<String>,
        /// delegation mode: direct (fixed assignee), bid (role members compete),
        /// broadcast (every role member gets a copy). Default: direct when
        /// --assignee given, else bid.
        #[arg(long, value_parser = ["direct", "bid", "broadcast"])]
        mode: Option<String>,
        /// executor kind: either (default, human or agent may do it),
        /// agent (must be done by an AI), or human (must be done by a person)
        #[arg(long, default_value = "either", value_parser = ["either", "agent", "human"])]
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
    /// Retract a claimed task (return it to the open bid pool). The user who
    /// claimed it may retract their own; a lead may retract any task. The task
    /// goes back to `assigned` and is re-broadcast so role members can claim
    /// again.
    Retract {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
    },
    /// Re-broadcast a task so role members can claim it again (team lead).
    /// No-op if the task is already open (`assigned` with no claimer).
    ReBid {
        /// task id
        #[arg(value_name = "ID")]
        id: String,
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
        /// filter by executor kind (either/agent/human)
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
/// `teamx direct` — member-to-member direct connectivity (data-plane rendezvous).
/// The server is the control plane: it stores candidates so same-team members
/// can try a direct TCP path before falling back to the tunnel relay.
#[derive(Subcommand, Debug)]
pub enum DirectCmd {
    /// Advertise this member's direct-connect candidates to the server
    Advertise {
        /// LAN IP(s) teammates may reach (repeatable)
        #[arg(long = "lan-ip")]
        lan_ips: Vec<String>,
        /// local direct-listener port
        #[arg(long)]
        port: u16,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// server URL (default: TEAMX_SERVER_URL or https://127.0.0.1:5781)
        #[arg(long)]
        server: Option<String>,
    },
    /// Show a peer member's advertised direct candidates (same team only)
    Candidates {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        server: Option<String>,
    },
    /// Probe a peer's candidates with a direct TCP connect (reports reachability)
    Probe {
        #[arg(value_name = "MEMBER_ID")]
        member_id: String,
        /// per-candidate connect timeout (ms)
        #[arg(long, default_value_t = 800)]
        timeout_ms: u64,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        server: Option<String>,
    },
    /// (provider, P1) Run the direct-connect mTLS listener and register it as
    /// a candidate; validates one-time pairing tickets from peers.
    Listen {
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// bind address (default 0.0.0.0:0 = random port)
        #[arg(long)]
        bind: Option<String>,
        /// LAN IP(s) to advertise (default: auto-detect via server route)
        #[arg(long = "lan-ip")]
        lan_ips: Vec<String>,
        #[arg(long)]
        server: Option<String>,
    },
    /// (consumer, P1) End-to-end direct ping: probe → pairing ticket → mTLS
    /// connect → ticket validation → RTT measurement.
    Ping {
        #[arg(value_name = "MEMBER_ID")]
        peer: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        team: Option<String>,
        /// number of ping/pong exchanges
        #[arg(long, default_value_t = 4)]
        count: u32,
        /// probe/connect timeout (ms)
        #[arg(long, default_value_t = 1500)]
        timeout_ms: u64,
        #[arg(long)]
        server: Option<String>,
    },
}

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
        /// Record flows/streams into capture.db.
        #[arg(long)]
        capture: bool,
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

/// `teamx capture` subcommands.
#[derive(Subcommand, Debug)]
pub enum CaptureCmd {
    /// Start the capture daemon (foreground; Ctrl-C to stop).
    Start {
        /// output database path (default ~/.teamx/capture.db)
        #[arg(long)]
        out: Option<PathBuf>,
        /// comma-separated sources to tap (tun0,socks5,forward; default all)
        #[arg(long)]
        sources: Option<String>,
    },
    /// Stop hint (the daemon is stopped via Ctrl-C; kept for symmetry).
    Stop,
    /// Show the capture database path and state.
    Status,
    /// List captured flows.
    List {
        /// filter by SNI glob (e.g. '*.google.com')
        #[arg(long)]
        sni: Option<String>,
        /// max rows (default 50)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Prune old flows to keep the DB under a size cap.
    Prune {
        /// size cap, e.g. 512MB (default 512MB)
        #[arg(long)]
        max_size: Option<String>,
    },
    /// Import an SSLKEYLOGFILE-format key log.
    ImportKeylog {
        /// path to the keylog file
        #[arg(long)]
        path: PathBuf,
    },
    /// Export a flow as pcap or all TLS secrets as an SSLKEYLOGFILE.
    Export {
        /// flow id to export as pcap (required with --pcap)
        #[arg(long)]
        flow: Option<i64>,
        /// output path for the pcap file
        #[arg(long)]
        pcap: Option<PathBuf>,
        /// output path for the SSLKEYLOGFILE
        #[arg(long)]
        keylog: Option<PathBuf>,
    },
}

/// `teamx analyze` subcommands (built on capture.db).
#[derive(Subcommand, Debug)]
pub enum AnalyzeCmd {
    /// Aggregate protocol / SNI / byte statistics.
    Stats,
    /// List captured flows.
    List {
        /// filter by SNI glob
        #[arg(long)]
        sni: Option<String>,
        /// max rows
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Reconstruct HTTP request/response headers for a flow.
    Http {
        /// flow id
        #[arg(long)]
        flow: i64,
    },
    /// Decrypt a TLS flow using imported traffic secrets.
    Decrypt {
        /// flow id
        #[arg(long)]
        flow: i64,
    },
}

/// `teamx replay` subcommands.
#[derive(Subcommand, Debug)]
pub enum ReplayCmd {
    /// Mock server: serve a flow's recorded down-stream on a local port.
    Serve {
        /// flow id
        #[arg(long)]
        flow: i64,
        /// local port (default 18080)
        #[arg(long, default_value_t = 18080)]
        port: u16,
    },
    /// TCP client replay + response diff.
    Tcp {
        #[command(subcommand)]
        cmd: ReplayTcpCmd,
    },
    /// HTTP exchange utilities (HAR export, to-curl, single send).
    Http {
        #[command(subcommand)]
        cmd: ReplayHttpCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReplayTcpCmd {
    /// Replay a flow's up-stream to a target and report the response diff.
    Start {
        /// flow id
        #[arg(long)]
        flow: i64,
        /// override target host:port
        #[arg(long)]
        to: Option<String>,
        /// proxy exit to tunnel through (default: routes default)
        #[arg(long)]
        exit: Option<String>,
        /// include a recorded-vs-replayed diff
        #[arg(long)]
        diff: bool,
        /// JSON result output path
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReplayHttpCmd {
    /// Extract a plain-HTTP exchange from a captured flow into http_exchanges.
    Extract {
        /// flow id
        #[arg(long)]
        flow: i64,
    },
    /// Export all exchanges as a HAR 1.2 document.
    ExportHar {
        /// output path
        #[arg(long, short = 'o')]
        out: PathBuf,
    },
    /// Print an equivalent curl command for an exchange.
    ToCurl {
        /// exchange id
        #[arg(long)]
        exchange: i64,
    },
    /// Re-send an exchange's request and report the response.
    Send {
        /// exchange id
        #[arg(long)]
        exchange: i64,
        /// variable substitution k=v (repeatable)
        #[arg(long = "var")]
        vars: Vec<String>,
        /// target rewrite from=to (repeatable)
        #[arg(long = "map")]
        maps: Vec<String>,
    /// JSON result output path
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    },
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
