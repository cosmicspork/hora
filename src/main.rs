use std::{
    io::Read,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hora::{
    client::{ApiError, Client, server_url},
    http::{self, App},
    kritee,
    store::Store,
};
use serde_json::{Map, Value, json};

#[derive(Parser)]
#[command(
    name = "hora",
    version,
    about = "Local time tracking: agents propose entries, the operator confirms them."
)]
struct Cli {
    /// The hora server, a loopback http:// origin.
    #[arg(
        long,
        global = true,
        env = "HORA_URL",
        default_value = "http://127.0.0.1:4530"
    )]
    server: String,
    /// The agent name recorded as the author of changes made through the CLI.
    #[arg(long, global = true, env = "HORA_AGENT", default_value = "unknown")]
    agent: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server and web UI.
    Serve {
        #[arg(long, default_value_t = 4530)]
        port: u16,
        #[arg(long, env = "HORA_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
    /// List the operations with their input schemas.
    Ops,
    /// Run any operation with JSON arguments (`-` reads them from stdin).
    Call { op: String, args: Option<String> },
    /// List projects.
    Projects,
    /// Record time (a draft until the operator confirms it).
    Log(LogArgs),
    /// List entries.
    List {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, value_enum)]
        state: Option<StateArg>,
    },
    /// Show one entry.
    Show { id: String },
    /// Show an entry's revisions.
    History { id: String },
    /// Change a draft. Give the version you read.
    Edit(EditArgs),
    /// Delete a draft.
    Delete {
        id: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Propose a change to a confirmed entry, for the operator to accept or reject.
    Propose(ProposeArgs),
    /// List proposals, newest first.
    Proposals {
        #[arg(long, value_enum)]
        state: Option<ProposalStateArg>,
        /// Only proposals for this entry id.
        #[arg(long)]
        entry: Option<String>,
    },
    /// Withdraw your pending proposal.
    Withdraw { id: String },
    /// Start a timer.
    Start {
        #[arg(long, short)]
        project: String,
        #[arg(long = "description", short = 'm')]
        description: String,
        #[arg(long = "ref")]
        refs: Vec<String>,
        /// Backdate the start.
        #[arg(long)]
        at: Option<String>,
    },
    /// Stop the running timer.
    Stop {
        #[arg(long = "description", short = 'm')]
        description: Option<String>,
        /// Correct the end.
        #[arg(long)]
        at: Option<String>,
    },
    /// Show the running timer.
    Status,
    /// Minutes by period and project.
    Summary {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, value_enum, default_value = "day")]
        by: ByArg,
    },
    /// Import from another tool. Entries arrive as drafts.
    #[command(subcommand)]
    Import(Import),
}

#[derive(Subcommand)]
enum Import {
    /// A kritee export directory (projects.jsonl, time_entries.jsonl).
    Kritee {
        dir: PathBuf,
        /// Where typed times were entered, e.g. America/New_York.
        #[arg(long)]
        timezone: String,
        /// From a date on, typed times were in another zone: 2026-06-20=Europe/Berlin.
        #[arg(long = "zone")]
        zones: Vec<String>,
        /// Print what would be sent without sending it.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
struct LogArgs {
    #[arg(long, short)]
    project: String,
    #[arg(long = "description", short = 'm')]
    description: String,
    #[arg(long)]
    date: Option<String>,
    #[arg(long)]
    start: Option<String>,
    #[arg(long)]
    end: Option<String>,
    #[arg(long)]
    minutes: Option<i64>,
    #[arg(long, overrides_with = "no_billable")]
    billable: bool,
    #[arg(long)]
    no_billable: bool,
    #[arg(long = "ref")]
    refs: Vec<String>,
    #[arg(long)]
    import_key: Option<String>,
    #[arg(long)]
    reason: Option<String>,
}

#[derive(Args)]
struct EditArgs {
    id: String,
    #[arg(long)]
    version: i64,
    #[arg(long)]
    project: Option<String>,
    #[arg(long = "description", short = 'm')]
    description: Option<String>,
    #[arg(long)]
    date: Option<String>,
    #[arg(long)]
    start: Option<String>,
    #[arg(long)]
    end: Option<String>,
    #[arg(long)]
    clear_times: bool,
    #[arg(long)]
    minutes: Option<i64>,
    #[arg(long, overrides_with = "no_billable")]
    billable: bool,
    #[arg(long)]
    no_billable: bool,
    /// Replaces all refs; repeat for several.
    #[arg(long = "ref")]
    refs: Vec<String>,
    #[arg(long)]
    reason: Option<String>,
}

#[derive(Args)]
struct ProposeArgs {
    #[command(flatten)]
    edit: EditArgs,
    /// Propose deleting the entry instead.
    #[arg(long, conflicts_with_all = ["project", "description", "date", "start", "end", "clear_times", "minutes", "billable", "no_billable", "refs"])]
    delete: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProposalStateArg {
    Pending,
    Accepted,
    Rejected,
    Withdrawn,
}

#[derive(Clone, Copy, ValueEnum)]
enum StateArg {
    Draft,
    Confirmed,
}

#[derive(Clone, Copy, ValueEnum)]
enum ByArg {
    Day,
    Week,
    Month,
}

/// Arguments for `time_edit`, or for `time_propose` when `delete` is given.
fn edit_args(a: EditArgs, delete: Option<bool>) -> Value {
    let mut pairs = vec![
        ("id", Some(a.id.into())),
        ("version", Some(a.version.into())),
        ("project", some(a.project)),
        ("description", some(a.description)),
        ("date", some(a.date)),
        ("started_at", some(a.start)),
        ("ended_at", some(a.end)),
        ("clear_times", a.clear_times.then_some(true.into())),
        ("minutes", some(a.minutes)),
        ("billable", some(billable(a.billable, a.no_billable))),
        ("refs", (!a.refs.is_empty()).then(|| a.refs.into())),
        ("reason", some(a.reason)),
    ];
    if let Some(delete) = delete {
        pairs.push(("delete", delete.then_some(true.into())));
    }
    object(pairs)
}

fn billable(yes: bool, no: bool) -> Option<bool> {
    match (yes, no) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

/// Builds a JSON object, leaving out absent values.
fn object(pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        if let Some(value) = value {
            map.insert(key.to_string(), value);
        }
    }
    Value::Object(map)
}

fn some<T: Into<Value>>(value: Option<T>) -> Option<Value> {
    value.map(Into::into)
}

fn print(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        match err.downcast_ref::<ApiError>() {
            Some(api) => {
                eprintln!("hora: {api}");
                if !api.body.is_null() {
                    eprintln!("{}", api.body);
                }
            }
            None => eprintln!("hora: {err:#}"),
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Serve { port, data_dir } = cli.command {
        return serve(port, data_dir).await;
    }
    let client = Client::new(server_url(&cli.server)?, cli.agent)?;
    let (op, args) = match cli.command {
        Command::Serve { .. } => unreachable!(),
        Command::Ops => {
            print(&client.ops().await?);
            return Ok(());
        }
        Command::Import(Import::Kritee {
            dir,
            timezone,
            zones,
            dry_run,
        }) => {
            return import_kritee(&client, &dir, &timezone, &zones, dry_run).await;
        }
        Command::Call { op, args } => {
            let raw = match args.as_deref() {
                None => "{}".to_string(),
                Some("-") => {
                    let mut buffer = String::new();
                    std::io::stdin().read_to_string(&mut buffer)?;
                    buffer
                }
                Some(raw) => raw.to_string(),
            };
            (
                op,
                serde_json::from_str(&raw).context("arguments must be JSON")?,
            )
        }
        Command::Projects => ("time_projects".into(), json!({})),
        Command::Log(a) => (
            "time_log".into(),
            object(vec![
                ("project", Some(a.project.into())),
                ("description", Some(a.description.into())),
                ("date", some(a.date)),
                ("started_at", some(a.start)),
                ("ended_at", some(a.end)),
                ("minutes", some(a.minutes)),
                ("billable", some(billable(a.billable, a.no_billable))),
                ("refs", Some(a.refs.into())),
                ("import_key", some(a.import_key)),
                ("reason", some(a.reason)),
            ]),
        ),
        Command::List {
            from,
            to,
            project,
            state,
        } => (
            "time_list".into(),
            object(vec![
                ("from", some(from)),
                ("to", some(to)),
                ("project", some(project)),
                (
                    "state",
                    state.map(|s| match s {
                        StateArg::Draft => "draft".into(),
                        StateArg::Confirmed => "confirmed".into(),
                    }),
                ),
            ]),
        ),
        Command::Show { id } => ("time_show".into(), json!({ "id": id })),
        Command::History { id } => ("time_history".into(), json!({ "id": id })),
        Command::Edit(a) => ("time_edit".into(), edit_args(a, None)),
        Command::Propose(p) => ("time_propose".into(), edit_args(p.edit, Some(p.delete))),
        Command::Proposals { state, entry } => (
            "time_proposals".into(),
            object(vec![
                (
                    "state",
                    state.map(|s| {
                        match s {
                            ProposalStateArg::Pending => "pending",
                            ProposalStateArg::Accepted => "accepted",
                            ProposalStateArg::Rejected => "rejected",
                            ProposalStateArg::Withdrawn => "withdrawn",
                        }
                        .into()
                    }),
                ),
                ("entry", some(entry)),
            ]),
        ),
        Command::Withdraw { id } => ("time_withdraw".into(), json!({ "id": id })),
        Command::Delete {
            id,
            version,
            reason,
        } => (
            "time_delete".into(),
            object(vec![
                ("id", Some(id.into())),
                ("version", Some(version.into())),
                ("reason", some(reason)),
            ]),
        ),
        Command::Start {
            project,
            description,
            refs,
            at,
        } => (
            "time_start".into(),
            object(vec![
                ("project", Some(project.into())),
                ("description", Some(description.into())),
                ("refs", Some(refs.into())),
                ("started_at", some(at)),
            ]),
        ),
        Command::Stop { description, at } => (
            "time_stop".into(),
            object(vec![
                ("description", some(description)),
                ("ended_at", some(at)),
            ]),
        ),
        Command::Status => ("time_status".into(), json!({})),
        Command::Summary {
            from,
            to,
            project,
            by,
        } => (
            "time_summary".into(),
            object(vec![
                ("from", Some(from.into())),
                ("to", Some(to.into())),
                ("project", some(project)),
                (
                    "by",
                    Some(
                        match by {
                            ByArg::Day => "day",
                            ByArg::Week => "week",
                            ByArg::Month => "month",
                        }
                        .into(),
                    ),
                ),
            ]),
        ),
    };
    print(&client.call(&op, args).await?);
    Ok(())
}

async fn serve(port: u16, data_dir: Option<PathBuf>) -> Result<()> {
    let data_dir = match data_dir {
        Some(dir) => dir,
        None => PathBuf::from(
            std::env::var_os("HOME").context("HOME is required unless --data-dir is set")?,
        )
        .join(".local/share/hora"),
    };
    let store = Arc::new(Store::open(&data_dir)?);
    // Socket activation hands over a listener; otherwise bind loopback.
    let mut fds = listenfd::ListenFd::from_env();
    let listener = match fds.take_tcp_listener(0)? {
        Some(listener) => {
            listener.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(listener)?
        }
        None => tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .await
            .context("binding the local hora port")?,
    };
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        bail!("hora listens on loopback only, not {address}");
    }
    let origin = format!("http://127.0.0.1:{}", address.port());
    println!("hora: http://localhost:{}", address.port());
    axum::serve(listener, http::router(App { store, origin }))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving hora")
}

async fn import_kritee(
    client: &Client,
    dir: &std::path::Path,
    timezone: &str,
    zones: &[String],
    dry_run: bool,
) -> Result<()> {
    let default = timezone
        .parse()
        .map_err(|e| anyhow::anyhow!("--timezone: {e}"))?;
    let ranges = zones
        .iter()
        .map(|z| kritee::Zones::parse_range(z))
        .collect::<Result<Vec<_>>>()?;
    let planned = kritee::plan(dir, &kritee::Zones::new(default, ranges))?;
    if dry_run {
        for p in &planned {
            println!("kritee {:>3} [{}] {}", p.kritee_id, p.zone, p.input);
        }
        println!("{} entries would be sent as drafts.", planned.len());
        return Ok(());
    }
    let projects = client.call("time_projects", json!({})).await?;
    let known: Vec<&str> = projects
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["slug"].as_str()).collect())
        .unwrap_or_default();
    let mut missing: Vec<&str> = planned
        .iter()
        .filter_map(|p| p.input["project"].as_str())
        .filter(|slug| !known.contains(slug))
        .collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        bail!(
            "add these projects in the hora UI first: {}",
            missing.join(", ")
        );
    }
    let (mut created, mut skipped, mut failed) = (0, 0, 0);
    for p in planned {
        match client.call("time_log", p.input).await {
            Ok(_) => created += 1,
            Err(err) => match err.downcast_ref::<ApiError>() {
                Some(api) if api.code() == "duplicate" => skipped += 1,
                _ => {
                    failed += 1;
                    eprintln!("kritee {}: {err}", p.kritee_id);
                }
            },
        }
    }
    println!("{created} drafts created, {skipped} already imported, {failed} failed.");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
