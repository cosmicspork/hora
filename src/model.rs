//! Shapes shared by the store, the operation registry, the HTTP API and the CLI.
//! Every input is an operation's argument and derives its JSON schema, which the
//! registry publishes and a Tracon plugin manifest will reuse.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Who is making a change. Decided only by the HTTP layer (`http::actor_for`),
/// so wave 2 can substitute Tracon's signed caller context in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    Operator,
    Agent(String),
}

impl Actor {
    pub fn label(&self) -> String {
        match self {
            Self::Operator => "operator".into(),
            Self::Agent(name) => format!("agent:{name}"),
        }
    }

    pub fn via(&self) -> &'static str {
        match self {
            Self::Operator => "ui",
            Self::Agent(_) => "agent",
        }
    }

    pub fn is_operator(&self) -> bool {
        matches!(self, Self::Operator)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    Draft,
    Confirmed,
}

impl EntryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Confirmed => "confirmed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "confirmed" => Some(Self::Confirmed),
            _ => None,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Project {
    pub slug: String,
    pub name: String,
    pub client: Option<String>,
    pub billable_default: bool,
    pub archived: bool,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub project: String,
    pub project_name: String,
    pub date: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// Null while the timer runs.
    pub minutes: Option<i64>,
    pub running: bool,
    pub description: String,
    pub billable: bool,
    pub state: EntryState,
    pub author: String,
    pub refs: Vec<String>,
    pub import_key: Option<String>,
    pub deleted: bool,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
    /// Agents' proposed changes waiting on the operator.
    #[serde(skip_serializing_if = "is_zero")]
    pub pending_proposals: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Serialize, Clone, Debug)]
pub struct Revision {
    pub id: i64,
    pub entry_id: String,
    pub version: i64,
    pub actor: String,
    pub via: String,
    pub action: String,
    /// `{field: [before, after]}` for each field the change touched.
    pub changes: serde_json::Value,
    pub reason: Option<String>,
    pub at: String,
    /// Set when the operator accepted an agent's proposal: which one, and who
    /// proposed it. `actor` is then the operator who approved it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProposalState {
    Pending,
    Accepted,
    Rejected,
    Withdrawn,
}

impl ProposalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// An agent's proposed change to an entry it may not edit itself.
#[derive(Serialize, Clone, Debug)]
pub struct Proposal {
    pub id: String,
    pub entry_id: String,
    /// The entry version the proposal was made against.
    pub entry_version: i64,
    /// `update` or `delete`.
    pub action: String,
    /// `{field: [before, after]}` against `entry_version`.
    pub changes: serde_json::Value,
    pub reason: String,
    pub author: String,
    pub state: ProposalState,
    /// The operator's note on accepting or rejecting.
    pub note: Option<String>,
    pub decided_by: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
    /// Pending, but the entry changed since, so it can't be accepted.
    pub stale: bool,
    /// The entry as it is now.
    pub entry: Entry,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Confirmed, billable.
    pub billable_minutes: i64,
    /// Confirmed, not billable.
    pub nonbillable_minutes: i64,
    /// Unconfirmed, either kind.
    pub draft_minutes: i64,
    pub entries: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct SummaryRow {
    pub period: String,
    pub project: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Serialize, Clone, Debug)]
pub struct Summary {
    pub from: String,
    pub to: String,
    pub by: Grouping,
    pub rows: Vec<SummaryRow>,
    pub total: Totals,
}

#[derive(Serialize, Clone, Debug)]
pub struct State {
    pub today: String,
    pub utc_offset: String,
    pub projects: Vec<Project>,
    pub running: Option<Entry>,
    pub drafts: i64,
    /// Pending proposals.
    pub proposals: i64,
}

// ---- Operation inputs -------------------------------------------------------

/// Record time against a project. Agents create drafts; the operator confirms.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct LogEntry {
    /// Project slug, such as `acme`.
    pub project: String,
    /// Local date, `YYYY-MM-DD`. Defaults to the start's date, else today.
    #[serde(default)]
    pub date: Option<String>,
    /// Start, RFC 3339 with offset, or `YYYY-MM-DD HH:MM` in the server's local time.
    #[serde(default)]
    pub started_at: Option<String>,
    /// End, same formats as `started_at`. Requires `started_at`.
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Minutes worked, 1 to 1440. Defaults to the span between start and end;
    /// may be less than the span (breaks), never more.
    #[serde(default)]
    pub minutes: Option<i64>,
    pub description: String,
    /// Defaults to the project's setting.
    #[serde(default)]
    pub billable: Option<bool>,
    /// Evidence and links: `workboard:<id>`, `jira:<KEY>`, `pr:<repo>#<n>`,
    /// `commit:<repo>@<sha>`, `kritee:<id>`, `tracon:<id>`, or an http(s) URL.
    #[serde(default)]
    pub refs: Vec<String>,
    /// A caller-chosen unique key; logging the same key twice is refused as a
    /// duplicate, so an import or backfill can be re-run safely.
    #[serde(default)]
    pub import_key: Option<String>,
    /// Why, recorded on the revision.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Change an entry. Only the fields given change. Agents may change drafts only.
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct EditEntry {
    pub id: String,
    /// The version you read; a stale version is refused.
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Remove the start and end, keeping the minutes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_times: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billable: Option<bool>,
    /// Replaces the refs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Propose a change to a confirmed entry, which agents can't edit. The
/// operator accepts or rejects it; accepting applies it as an operator edit.
/// Give the fields to change, or `delete`. Drafts are edited directly instead.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ProposeChange {
    pub id: String,
    /// The version you read; the proposal goes stale if the entry changes.
    pub version: i64,
    /// Propose deleting the entry instead of changing it.
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Remove the start and end, keeping the minutes.
    #[serde(default)]
    pub clear_times: bool,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub billable: Option<bool>,
    /// Replaces the refs.
    #[serde(default)]
    pub refs: Option<Vec<String>>,
    /// Why, for the operator. Required.
    pub reason: String,
}

impl ProposeChange {
    /// The change as an edit, which is how it is checked and applied.
    pub fn as_edit(&self) -> EditEntry {
        EditEntry {
            id: self.id.clone(),
            version: self.version,
            project: self.project.clone(),
            date: self.date.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            clear_times: self.clear_times,
            minutes: self.minutes,
            description: self.description.clone(),
            billable: self.billable,
            refs: self.refs.clone(),
            reason: None,
        }
    }
}

/// List proposals, newest first. Without `state`, every state.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ListProposals {
    #[serde(default)]
    pub state: Option<ProposalState>,
    /// Only proposals for this entry id.
    #[serde(default)]
    pub entry: Option<String>,
}

/// Accept or reject a pending proposal. Operator only.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct DecideProposal {
    pub id: String,
    /// A note for the agent.
    #[serde(default)]
    pub note: Option<String>,
}

/// Withdraw your own pending proposal.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct WithdrawProposal {
    pub id: String,
}

/// Delete an entry (kept in history). Agents may delete drafts only.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeleteEntry {
    pub id: String,
    pub version: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct EntryVersion {
    pub id: String,
    pub version: i64,
}

/// Confirm drafts. Operator only; all or none.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ConfirmEntries {
    pub entries: Vec<EntryVersion>,
}

/// Read one entry, including a deleted one.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ShowEntry {
    pub id: String,
}

/// List entries by local date, oldest first. Defaults to the last seven days.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ListEntries {
    /// First date, inclusive, `YYYY-MM-DD`.
    #[serde(default)]
    pub from: Option<String>,
    /// Last date, inclusive, `YYYY-MM-DD`.
    #[serde(default)]
    pub to: Option<String>,
    /// Project slug.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub state: Option<EntryState>,
}

/// Start a timer. One timer runs at a time.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct StartTimer {
    pub project: String,
    pub description: String,
    #[serde(default)]
    pub billable: Option<bool>,
    #[serde(default)]
    pub refs: Vec<String>,
    /// Backdate the start (forgot to start it). Defaults to now.
    #[serde(default)]
    pub started_at: Option<String>,
}

/// Stop the running timer. Agents may stop only a timer an agent started.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct StopTimer {
    /// Replace the description.
    #[serde(default)]
    pub description: Option<String>,
    /// Correct the end (forgot to stop it). Defaults to now.
    #[serde(default)]
    pub ended_at: Option<String>,
}

/// No arguments.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Grouping {
    #[default]
    Day,
    Week,
    Month,
}

/// Minutes by period and project: confirmed billable, confirmed non-billable,
/// and drafts. Running timers are not counted.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SummaryQuery {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub by: Grouping,
}

/// Add a project. Operator only.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    /// Lowercase words joined by dashes; it never changes.
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub billable_default: Option<bool>,
}

/// Change a project. Operator only.
#[derive(Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct UpdateProject {
    pub slug: String,
    pub version: i64,
    #[serde(default)]
    pub name: Option<String>,
    /// An empty string clears it.
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub billable_default: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
}
