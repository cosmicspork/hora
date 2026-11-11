//! The operation registry: every action hora offers, named and described once,
//! with its JSON input schema. The agent API and the CLI are thin layers over
//! it, the web UI calls it as the operator, and in wave 2 it becomes the Tracon
//! plugin manifest's tool list.

use schemars::{JsonSchema, Schema, schema_for};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::{
    Actor, ConfirmEntries, CreateProject, DecideProposal, DeleteEntry, EditEntry, Empty,
    ListEntries, ListProposals, LogEntry, ProposeChange, ShowEntry, StartTimer, StopTimer,
    SummaryQuery, UpdateProject, WithdrawProposal,
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

/// Runs an operation as `actor`. The store enforces who may do what.
pub fn dispatch(store: &Store, actor: &Actor, name: &str, args: Value) -> Result<Value, OpError> {
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
        "time_summary" => out(store.summary(&input::<SummaryQuery>(args)?)?),
        "time_log" => out(store.log(actor, input(args)?)?),
        "time_edit" => out(store.edit(actor, input(args)?)?),
        "time_delete" => out(store.delete(actor, input(args)?)?),
        "time_start" => out(store.start(actor, input(args)?)?),
        "time_stop" => out(store.stop(actor, input(args)?)?),
        "time_propose" => out(store.propose(actor, input(args)?)?),
        "time_proposals" => out(store.proposals(&input::<ListProposals>(args)?)?),
        "time_withdraw" => out(store.withdraw(actor, input(args)?)?),
        "proposal_accept" => out(store.accept(actor, input(args)?)?),
        "proposal_reject" => out(store.reject(actor, input(args)?)?),
        "time_confirm" => out(store.confirm(actor, input(args)?)?),
        "project_create" => out(store.create_project(actor, input(args)?)?),
        "project_update" => out(store.update_project(actor, input(args)?)?),
        _ => Err(OpError::Unknown(name.to_string())),
    }
}
