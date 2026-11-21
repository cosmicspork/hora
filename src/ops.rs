//! The operation registry: every action hora offers, named and described once,
//! with its JSON input schema. The agent API and the CLI are thin layers over
//! it, the web UI calls it as the operator (every HTTP route is one of these,
//! see `http::ROUTES`), and as a Tracon plugin the ones an agent may run are
//! its MCP tools (`plugin::tools`). `tests/parity.rs` holds the three together.

use schemars::{JsonSchema, Schema, schema_for};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::{
    ConfirmEntries, CreateProject, DecideProposal, DeleteEntry, EditEntry, Empty, Entry, Export,
    ExportFormat, ExportQuery, ListEntries, ListProposals, LogEntry, ProposeChange, ShowEntry,
    StartTimer, StopTimer, SummaryQuery, UpdateProject, WithdrawProposal, Writer,
};
use crate::store::{Store, StoreError};

#[derive(Serialize)]
pub struct Op {
    pub name: &'static str,
    pub description: &'static str,
    /// Changes data. A hint for Tracon policy; hora's own gates decide.
    pub writes: bool,
    /// Refused for agents whatever the policy says.
    pub operator_only: bool,
    #[serde(skip)]
    schema: fn() -> Schema,
}

impl Op {
    pub fn input_schema(&self) -> Schema {
        (self.schema)()
    }

    pub fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "writes": self.writes,
            "operator_only": self.operator_only,
            "input_schema": self.input_schema(),
        })
    }
}

fn schema<T: JsonSchema>() -> Schema {
    schema_for!(T)
}

macro_rules! op {
    ($name:literal, $ty:ty, $writes:literal, $operator:literal, $description:literal) => {
        Op {
            name: $name,
            description: $description,
            writes: $writes,
            operator_only: $operator,
            schema: schema::<$ty>,
        }
    };
}

pub const OPS: &[Op] = &[
    op!(
        "time_overview",
        Empty,
        false,
        false,
        "Today's local date and UTC offset, the projects, the running timer, and how many drafts and pending proposals wait on the operator."
    ),
    op!(
        "time_projects",
        Empty,
        false,
        false,
        "List projects. Log time against a project by its slug."
    ),
    op!(
        "time_list",
        ListEntries,
        false,
        false,
        "List entries by local date range, project and state. Defaults to the last seven days."
    ),
    op!(
        "time_show",
        ShowEntry,
        false,
        false,
        "Read one entry with its warnings."
    ),
    op!(
        "time_history",
        ShowEntry,
        false,
        false,
        "Read an entry's revisions: who changed what, when, and why."
    ),
    op!(
        "time_status",
        Empty,
        false,
        false,
        "The running timer, if any."
    ),
    op!(
        "time_summary",
        SummaryQuery,
        false,
        false,
        "Minutes by day, week or month and project, split into confirmed billable, confirmed non-billable, and drafts."
    ),
    op!(
        "time_export",
        ExportQuery,
        false,
        false,
        "Export entries by local date range and project as CSV or JSONL, without a running timer. Returns the file name, media type and text."
    ),
    op!(
        "time_log",
        LogEntry,
        true,
        false,
        "Record time. An agent's entry is a draft until the operator confirms it. Cite evidence in refs."
    ),
    op!(
        "time_edit",
        EditEntry,
        true,
        false,
        "Change an entry's fields. Agents may change drafts only; for a confirmed entry, use time_propose."
    ),
    op!(
        "time_delete",
        DeleteEntry,
        true,
        false,
        "Delete an entry; history is kept. Agents may delete drafts only."
    ),
    op!(
        "time_start",
        StartTimer,
        true,
        false,
        "Start a timer. One runs at a time; an agent's timer is a draft."
    ),
    op!(
        "time_stop",
        StopTimer,
        true,
        false,
        "Stop the running timer. Agents may stop only an agent's timer."
    ),
    op!(
        "time_propose",
        ProposeChange,
        true,
        false,
        "Propose a change to, or the deletion of, a confirmed entry. The operator accepts or rejects it. One pending proposal per entry per agent."
    ),
    op!(
        "time_proposals",
        ListProposals,
        false,
        false,
        "List proposals, newest first, with the entry as it is now. `stale` means the entry changed since, so it can't be accepted."
    ),
    op!(
        "time_withdraw",
        WithdrawProposal,
        true,
        false,
        "Withdraw your own pending proposal."
    ),
    op!(
        "proposal_accept",
        DecideProposal,
        true,
        true,
        "Apply a pending proposal as an operator edit. Operator only."
    ),
    op!(
        "proposal_reject",
        DecideProposal,
        true,
        true,
        "Reject a pending proposal. Operator only."
    ),
    op!(
        "time_confirm",
        ConfirmEntries,
        true,
        true,
        "Confirm drafts. Operator only."
    ),
    op!(
        "project_create",
        CreateProject,
        true,
        true,
        "Add a project. Operator only."
    ),
    op!(
        "project_update",
        UpdateProject,
        true,
        true,
        "Change or archive a project. Operator only."
    ),
];

pub fn find(name: &str) -> Option<&'static Op> {
    OPS.iter().find(|op| op.name == name)
}

#[derive(Debug)]
pub enum OpError {
    Unknown(String),
    BadInput(String),
    Store(StoreError),
}

impl From<StoreError> for OpError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "no operation `{name}`"),
            Self::BadInput(message) => f.write_str(message),
            Self::Store(err) => err.fmt(f),
        }
    }
}

fn input<T: DeserializeOwned>(value: Value) -> Result<T, OpError> {
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value).map_err(|e| OpError::BadInput(format!("invalid input: {e}")))
}

fn out<T: Serialize>(value: T) -> Result<Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::Store(StoreError::Internal(e.to_string())))
}

/// Runs an operation as `by`: its actor, and the Tracon session recorded on
/// what it writes. The store enforces who may do what.
pub fn dispatch<'a>(
    store: &Store,
    by: impl Into<Writer<'a>>,
    name: &str,
    args: Value,
) -> Result<Value, OpError> {
    let by = by.into();
    let actor = by.actor;
    let op = find(name).ok_or_else(|| OpError::Unknown(name.to_string()))?;
    if op.operator_only && !actor.is_operator() {
        return Err(OpError::Store(StoreError::Forbidden(format!(
            "`{name}` is for the operator only."
        ))));
    }
    match name {
        "time_projects" => {
            input::<Empty>(args)?;
            out(store.projects()?)
        }
        "time_list" => out(store.entries(&input::<ListEntries>(args)?)?),
        "time_show" => out(store.entry(&input::<ShowEntry>(args)?.id)?),
        "time_history" => out(store.history(&input::<ShowEntry>(args)?.id)?),
        "time_status" => {
            input::<Empty>(args)?;
            out(json!({ "running": store.running()? }))
        }
        "time_overview" => {
            input::<Empty>(args)?;
            out(store.state()?)
        }
        "time_summary" => out(store.summary(&input::<SummaryQuery>(args)?)?),
        "time_export" => out(export(store, input(args)?)?),
        "time_log" => out(store.log(by, input(args)?)?),
        "time_edit" => out(store.edit(by, input(args)?)?),
        "time_delete" => out(store.delete(by, input(args)?)?),
        "time_start" => out(store.start(by, input(args)?)?),
        "time_stop" => out(store.stop(by, input(args)?)?),
        "time_propose" => out(store.propose(by, input(args)?)?),
        "time_proposals" => out(store.proposals(&input::<ListProposals>(args)?)?),
        "time_withdraw" => out(store.withdraw(by, input(args)?)?),
        "proposal_accept" => out(store.accept(by, input(args)?)?),
        "proposal_reject" => out(store.reject(by, input(args)?)?),
        "time_confirm" => out(store.confirm(by, input(args)?)?),
        "project_create" => out(store.create_project(actor, input(args)?)?),
        "project_update" => out(store.update_project(actor, input(args)?)?),
        _ => Err(OpError::Unknown(name.to_string())),
    }
}

fn export(store: &Store, query: ExportQuery) -> Result<Export, OpError> {
    let filter = ListEntries {
        from: query.from,
        to: query.to,
        project: query.project,
        state: None,
    };
    let entries: Vec<Entry> = store
        .entries(&filter)?
        .into_iter()
        .filter(|e| !e.running)
        .collect();
    let (content, content_type, extension) = match query.format {
        ExportFormat::Csv => (to_csv(&entries), "text/csv; charset=utf-8", "csv"),
        ExportFormat::Jsonl => (
            to_jsonl(&entries),
            "application/x-ndjson; charset=utf-8",
            "jsonl",
        ),
    };
    let filename = format!(
        "hora-{}-{}.{extension}",
        entries.first().map(|e| e.date.as_str()).unwrap_or("empty"),
        entries.last().map(|e| e.date.as_str()).unwrap_or("empty"),
    );
    Ok(Export {
        format: query.format,
        filename,
        content_type: content_type.to_string(),
        entries: entries.len(),
        content,
    })
}

fn to_jsonl(entries: &[Entry]) -> String {
    let mut body = String::new();
    for entry in entries {
        let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.remove("warnings");
        }
        body.push_str(&value.to_string());
        body.push('\n');
    }
    body
}

pub fn to_csv(entries: &[Entry]) -> String {
    fn cell(value: &str) -> String {
        if value.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_string()
        }
    }
    let mut out = String::from(
        "id,date,project,started_at,ended_at,minutes,hours,billable,state,author,description,refs\n",
    );
    for e in entries {
        let minutes = e.minutes.unwrap_or(0);
        let row = [
            e.id.clone(),
            e.date.clone(),
            e.project.clone(),
            e.started_at.clone().unwrap_or_default(),
            e.ended_at.clone().unwrap_or_default(),
            minutes.to_string(),
            format!("{:.2}", minutes as f64 / 60.0),
            e.billable.to_string(),
            e.state.as_str().to_string(),
            e.author.clone(),
            e.description.clone(),
            e.refs.join(" "),
        ];
        out.push_str(&row.iter().map(|v| cell(v)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}
