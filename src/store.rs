//! SQLite persistence and the gates. Every rule about who may change what lives
//! here, so the operator UI, the agent API and the CLI all meet the same checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::{
    Actor, ConfirmEntries, CreateProject, DecideProposal, DeleteEntry, EditEntry, Entry,
    EntryState, Grouping, ListEntries, ListProposals, LogEntry, Project, Proposal, ProposalState,
    ProposeChange, Revision, StartTimer, State, StopTimer, Summary, SummaryQuery, SummaryRow,
    Totals, UpdateProject, Warning, WithdrawProposal,
};
use crate::time;

pub const MAX_MINUTES: i64 = 1440;
pub const LONG_MINUTES: i64 = 600;
const MAX_REFS: usize = 20;
const MAX_TEXT: usize = 2000;
const REF_KINDS: &[&str] = &[
    "workboard",
    "jira",
    "pr",
    "commit",
    "kritee",
    "tracon",
    "issue",
];

#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    NotFound,
    Conflict,
    Forbidden(String),
    Duplicate(String),
    TimerRunning(String),
    /// The entry changed since the proposal was made.
    Stale(String),
    /// The agent already has a pending proposal for this entry.
    PendingProposal(String),
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::Forbidden(message)
            | Self::Stale(message)
            | Self::Internal(message) => f.write_str(message),
            Self::NotFound => f.write_str("not found"),
            Self::Conflict => f.write_str("this changed since it was read; reload it"),
            Self::Duplicate(id) => write!(f, "an entry with this import key exists: {id}"),
            Self::TimerRunning(id) => write!(f, "a timer is already running: {id}"),
            Self::PendingProposal(id) => write!(
                f,
                "you already have a pending proposal for this entry: {id}; withdraw it first"
            ),
        }
    }
}
impl std::error::Error for StoreError {}

fn internal(err: impl std::fmt::Display) -> StoreError {
    StoreError::Internal(err.to_string())
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Schema migrations, applied in order; `PRAGMA user_version` counts them.
pub const MIGRATIONS: &[&str] = &["
CREATE TABLE project(
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    client TEXT,
    billable_default INTEGER NOT NULL CHECK(billable_default IN (0,1)),
    archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
    version INTEGER NOT NULL DEFAULT 1 CHECK(version>0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE entry(
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES project(id),
    date TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT,
    start_ms INTEGER,
    end_ms INTEGER,
    minutes INTEGER CHECK(minutes IS NULL OR minutes BETWEEN 1 AND 1440),
    description TEXT NOT NULL,
    billable INTEGER NOT NULL CHECK(billable IN (0,1)),
    state TEXT NOT NULL CHECK(state IN ('draft','confirmed')),
    author TEXT NOT NULL,
    refs_json TEXT NOT NULL DEFAULT '[]',
    import_key TEXT UNIQUE,
    deleted_at TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version>0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(ended_at IS NULL OR started_at IS NOT NULL),
    CHECK(minutes IS NOT NULL OR (started_at IS NOT NULL AND ended_at IS NULL))
);
CREATE INDEX entry_date ON entry(date);
CREATE INDEX entry_start ON entry(start_ms);
-- At most one running timer: every running row indexes to the same value.
CREATE UNIQUE INDEX entry_one_running ON entry((minutes IS NULL)) WHERE minutes IS NULL AND deleted_at IS NULL;
CREATE TABLE revision(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id TEXT NOT NULL REFERENCES entry(id),
    version INTEGER NOT NULL,
    actor TEXT NOT NULL,
    via TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('create','update','confirm','delete','start','stop')),
    changes_json TEXT NOT NULL,
    reason TEXT,
    at TEXT NOT NULL
);
CREATE INDEX revision_entry ON revision(entry_id, id);
", "
CREATE TABLE proposal(
    id TEXT PRIMARY KEY,
    entry_id TEXT NOT NULL REFERENCES entry(id),
    entry_version INTEGER NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('update','delete')),
    input_json TEXT NOT NULL,
    changes_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    author TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','accepted','rejected','withdrawn')),
    note TEXT,
    decided_by TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX proposal_entry ON proposal(entry_id, state);
-- One pending proposal per entry per author.
CREATE UNIQUE INDEX proposal_one_pending ON proposal(entry_id, author) WHERE state='pending';
ALTER TABLE revision ADD COLUMN proposal_id TEXT REFERENCES proposal(id);
ALTER TABLE revision ADD COLUMN proposed_by TEXT;
"];

pub struct Store {
    connection: Mutex<Connection>,
}

/// The editable content of an entry, resolved and checked.
#[derive(Clone, Debug)]
struct Content {
    project_id: String,
    project_slug: String,
    date: NaiveDate,
    start: Option<DateTime<FixedOffset>>,
    end: Option<DateTime<FixedOffset>>,
    minutes: Option<i64>,
    description: String,
    billable: bool,
    refs: Vec<String>,
}

impl Content {
    fn running(&self) -> bool {
        self.minutes.is_none()
    }

    fn start_ms(&self) -> Option<i64> {
        self.start.map(|s| s.timestamp_millis())
    }

    /// The effective end for overlap checks; null while running.
    fn end_ms(&self) -> Option<i64> {
        match (self.start, self.end, self.minutes) {
            (_, Some(end), _) => Some(end.timestamp_millis()),
            (Some(start), None, Some(minutes)) => Some(start.timestamp_millis() + minutes * 60_000),
            _ => None,
        }
    }

    fn snapshot(&self, state: EntryState, deleted: bool) -> Value {
        json!({
            "project": self.project_slug,
            "date": self.date.to_string(),
            "started_at": self.start.as_ref().map(time::format),
            "ended_at": self.end.as_ref().map(time::format),
            "minutes": self.minutes,
            "description": self.description,
            "billable": self.billable,
            "refs": self.refs,
            "state": state.as_str(),
            "deleted": deleted,
        })
    }
}

struct Row {
    id: String,
    content: Content,
    project_name: String,
    state: EntryState,
    author: String,
    import_key: Option<String>,
    deleted: bool,
    version: i64,
    created_at: String,
    updated_at: String,
}

const ROW_SELECT: &str = "SELECT e.id,e.project_id,p.slug,p.name,e.date,e.started_at,e.ended_at,e.minutes,e.description,e.billable,e.state,e.author,e.refs_json,e.import_key,e.deleted_at,e.version,e.created_at,e.updated_at FROM entry e JOIN project p ON p.id=e.project_id";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let instant = |idx: usize| -> rusqlite::Result<Option<DateTime<FixedOffset>>> {
        let raw: Option<String> = row.get(idx)?;
        raw.map(|raw| {
            DateTime::parse_from_rfc3339(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    idx,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()
    };
    let date: String = row.get(4)?;
    let refs: String = row.get(12)?;
    let state: String = row.get(10)?;
    Ok(Row {
        id: row.get(0)?,
        content: Content {
            project_id: row.get(1)?,
            project_slug: row.get(2)?,
            date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            start: instant(5)?,
            end: instant(6)?,
            minutes: row.get(7)?,
            description: row.get(8)?,
            billable: row.get(9)?,
            refs: serde_json::from_str(&refs).unwrap_or_default(),
        },
        project_name: row.get(3)?,
        state: EntryState::parse(&state).unwrap_or(EntryState::Draft),
        author: row.get(11)?,
        import_key: row.get(13)?,
        deleted: row.get::<_, Option<String>>(14)?.is_some(),
        version: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

impl Row {
    fn into_entry(self, warnings: Vec<Warning>, pending_proposals: i64) -> Entry {
        let c = self.content;
        Entry {
            running: c.running(),
            id: self.id,
            project: c.project_slug,
            project_name: self.project_name,
            date: c.date.to_string(),
            started_at: c.start.as_ref().map(time::format),
            ended_at: c.end.as_ref().map(time::format),
            minutes: c.minutes,
            description: c.description,
            billable: c.billable,
            state: self.state,
            author: self.author,
            refs: c.refs,
            import_key: self.import_key,
            deleted: self.deleted,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
            warnings,
            pending_proposals,
        }
    }
}

impl Store {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating data directory {}", dir.display()))?;
        set_mode(dir, 0o700)?;
        let path = dir.join("hora.sqlite3");
        let mut connection = Connection::open(&path).context("opening hora database")?;
        set_mode(&path, 0o600)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut connection)?;
        // SQLite creates sidecars lazily; restrict any that exist on every open.
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
            if sidecar.exists() {
                set_mode(&sidecar, 0o600)?;
            }
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Internal("database lock poisoned".into()))
    }

    // ---- Projects ----------------------------------------------------------

    pub fn projects(&self) -> Result<Vec<Project>, StoreError> {
        let connection = self.lock()?;
        read_projects(&connection)
    }

    pub fn create_project(
        &self,
        actor: &Actor,
        input: CreateProject,
    ) -> Result<Project, StoreError> {
        operator_only(actor, "Only the operator adds projects.")?;
        let slug = input.slug.trim();
        validate_slug(slug)?;
        let name = required_text("name", &input.name, 200)?;
        let client = optional_text("client", input.client.as_deref(), 200)?;
        let connection = self.lock()?;
        let exists: bool = connection
            .query_row("SELECT 1 FROM project WHERE slug=?1", [slug], |_| Ok(true))
            .optional()
            .map_err(internal)?
            .unwrap_or(false);
        if exists {
            return Err(invalid(format!("project `{slug}` already exists")));
        }
        let now = time::utc_now();
        connection
            .execute(
                "INSERT INTO project(id,slug,name,client,billable_default,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",
                params![Uuid::now_v7().to_string(), slug, name, client, input.billable_default.unwrap_or(true), now],
            )
            .map_err(internal)?;
        read_project(&connection, slug)?.ok_or(StoreError::NotFound)
    }

    pub fn update_project(
        &self,
        actor: &Actor,
        input: UpdateProject,
    ) -> Result<Project, StoreError> {
        operator_only(actor, "Only the operator changes projects.")?;
        let connection = self.lock()?;
        let existing = read_project(&connection, &input.slug)?.ok_or(StoreError::NotFound)?;
        if existing.version != input.version {
            return Err(StoreError::Conflict);
        }
        let name = match input.name {
            Some(name) => required_text("name", &name, 200)?,
            None => existing.name,
        };
        let client = match input.client {
            Some(client) => optional_text("client", Some(&client), 200)?,
            None => existing.client,
        };
        connection
            .execute(
                "UPDATE project SET name=?1,client=?2,billable_default=?3,archived=?4,version=version+1,updated_at=?5 WHERE slug=?6",
                params![
                    name,
                    client,
                    input.billable_default.unwrap_or(existing.billable_default),
                    input.archived.unwrap_or(existing.archived),
                    time::utc_now(),
                    input.slug
                ],
            )
            .map_err(internal)?;
        read_project(&connection, &input.slug)?.ok_or(StoreError::NotFound)
    }

    // ---- Reads ------------------------------------------------------------

    pub fn state(&self) -> Result<State, StoreError> {
        let connection = self.lock()?;
        let projects = read_projects(&connection)?;
        let running = running_row(&connection)?
            .map(|row| with_warnings(&connection, row))
            .transpose()?;
        let drafts = connection
            .query_row(
                "SELECT count(*) FROM entry WHERE state='draft' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let proposals = connection
            .query_row(
                "SELECT count(*) FROM proposal WHERE state='pending'",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        Ok(State {
            today: time::today().to_string(),
            utc_offset: time::now().offset().to_string(),
            projects,
            running,
            drafts,
            proposals,
        })
    }

    pub fn running(&self) -> Result<Option<Entry>, StoreError> {
        let connection = self.lock()?;
        running_row(&connection)?
            .map(|row| with_warnings(&connection, row))
            .transpose()
    }

    pub fn entry(&self, id: &str) -> Result<Entry, StoreError> {
        let connection = self.lock()?;
        let row = load_row(&connection, id)?.ok_or(StoreError::NotFound)?;
        with_warnings(&connection, row)
    }

    pub fn entries(&self, filter: &ListEntries) -> Result<Vec<Entry>, StoreError> {
        let to = match &filter.to {
            Some(to) => time::parse_date(to).map_err(invalid)?,
            None => time::today(),
        };
        let from = match &filter.from {
            Some(from) => time::parse_date(from).map_err(invalid)?,
            None => to - chrono::Duration::days(6),
        };
        if from > to {
            return Err(invalid("`from` is after `to`"));
        }
        let connection = self.lock()?;
        let project_id = match &filter.project {
            Some(slug) => Some(project_id(&connection, slug)?),
            None => None,
        };
        let mut statement = connection
            .prepare(&format!(
                "{ROW_SELECT} WHERE e.deleted_at IS NULL AND e.date BETWEEN ?1 AND ?2 AND (?3 IS NULL OR e.project_id=?3) AND (?4 IS NULL OR e.state=?4) ORDER BY e.date, e.start_ms IS NULL, e.start_ms, e.created_at, e.id"
            ))
            .map_err(internal)?;
        let rows = statement
            .query_map(
                params![
                    from.to_string(),
                    to.to_string(),
                    project_id,
                    filter.state.map(EntryState::as_str)
                ],
                row_from,
            )
            .map_err(internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(internal)?;
        rows.into_iter()
            .map(|row| with_warnings(&connection, row))
            .collect()
    }

    pub fn history(&self, id: &str) -> Result<Vec<Revision>, StoreError> {
        let connection = self.lock()?;
        load_row(&connection, id)?.ok_or(StoreError::NotFound)?;
        let mut statement = connection
            .prepare("SELECT id,entry_id,version,actor,via,action,changes_json,reason,at,proposal_id,proposed_by FROM revision WHERE entry_id=?1 ORDER BY id")
            .map_err(internal)?;
        statement
            .query_map([id], |r| {
                let changes: String = r.get(6)?;
                Ok(Revision {
                    id: r.get(0)?,
                    entry_id: r.get(1)?,
                    version: r.get(2)?,
                    actor: r.get(3)?,
                    via: r.get(4)?,
                    action: r.get(5)?,
                    changes: serde_json::from_str(&changes).unwrap_or(Value::Null),
                    reason: r.get(7)?,
                    at: r.get(8)?,
                    proposal_id: r.get(9)?,
                    proposed_by: r.get(10)?,
                })
            })
            .map_err(internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(internal)
    }

    pub fn summary(&self, query: &SummaryQuery) -> Result<Summary, StoreError> {
        let from = time::parse_date(&query.from).map_err(invalid)?;
        let to = time::parse_date(&query.to).map_err(invalid)?;
        if from > to {
            return Err(invalid("`from` is after `to`"));
        }
        let connection = self.lock()?;
        let project_id = match &query.project {
            Some(slug) => Some(project_id(&connection, slug)?),
            None => None,
        };
        let mut statement = connection
            .prepare("SELECT e.date,p.slug,e.minutes,e.billable,e.state FROM entry e JOIN project p ON p.id=e.project_id WHERE e.deleted_at IS NULL AND e.minutes IS NOT NULL AND e.date BETWEEN ?1 AND ?2 AND (?3 IS NULL OR e.project_id=?3)")
            .map_err(internal)?;
        let mut groups: BTreeMap<(String, String), Totals> = BTreeMap::new();
        let mut total = Totals::default();
        let rows = statement
            .query_map(params![from.to_string(), to.to_string(), project_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(internal)?;
        for row in rows {
            let (date, slug, minutes, billable, state) = row.map_err(internal)?;
            let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(internal)?;
            let totals = groups.entry((period(date, query.by), slug)).or_default();
            for t in [&mut *totals, &mut total] {
                t.entries += 1;
                match (state.as_str(), billable) {
                    ("confirmed", true) => t.billable_minutes += minutes,
                    ("confirmed", false) => t.nonbillable_minutes += minutes,
                    _ => t.draft_minutes += minutes,
                }
            }
        }
        Ok(Summary {
            from: from.to_string(),
            to: to.to_string(),
            by: query.by,
            rows: groups
                .into_iter()
                .map(|((period, project), totals)| SummaryRow {
                    period,
                    project,
                    totals,
                })
                .collect(),
            total,
        })
    }

    // ---- Writes -----------------------------------------------------------

    pub fn log(&self, actor: &Actor, input: LogEntry) -> Result<Entry, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let (project_id, project_slug, billable_default) = active_project(&tx, &input.project)?;
        let start = input
            .started_at
            .as_deref()
            .map(time::parse_instant)
            .transpose()
            .map_err(invalid)?;
        let end = input
            .ended_at
            .as_deref()
            .map(time::parse_instant)
            .transpose()
            .map_err(invalid)?;
        let date = match (&input.date, start) {
            (Some(date), _) => time::parse_date(date).map_err(invalid)?,
            (None, Some(start)) => start.date_naive(),
            (None, None) => time::today(),
        };
        let minutes = match (input.minutes, start, end) {
            (Some(minutes), _, _) => Some(minutes),
            (None, Some(start), Some(end)) => Some(time::span_minutes(&start, &end).max(1)),
            _ => {
                return Err(invalid(
                    "give `minutes`, or both `started_at` and `ended_at`",
                ));
            }
        };
        let content = Content {
            project_id,
            project_slug,
            date,
            start,
            end,
            minutes,
            description: input.description.trim().to_string(),
            billable: input.billable.unwrap_or(billable_default),
            refs: normalize_refs(&input.refs)?,
        };
        validate(&content)?;
        let import_key = optional_text("import_key", input.import_key.as_deref(), 200)?;
        if let Some(key) = &import_key {
            let existing: Option<String> = tx
                .query_row("SELECT id FROM entry WHERE import_key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(internal)?;
            if let Some(id) = existing {
                return Err(StoreError::Duplicate(id));
            }
        }
        let reason = optional_text("reason", input.reason.as_deref(), MAX_TEXT)?;
        let state = initial_state(actor);
        let id = insert(&tx, actor, &content, state, import_key.as_deref())?;
        revise(
            &tx,
            &id,
            1,
            actor,
            "create",
            &Value::Null,
            &content.snapshot(state, false),
            reason.as_deref(),
            None,
        )?;
        let row = load_row(&tx, &id)?.ok_or(StoreError::NotFound)?;
        let entry = with_warnings(&tx, row)?;
        tx.commit().map_err(internal)?;
        Ok(entry)
    }

    pub fn edit(&self, actor: &Actor, input: EditEntry) -> Result<Entry, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let row = load_live(&tx, &input.id, input.version)?;
        if !actor.is_operator() && row.state == EntryState::Confirmed {
            return Err(StoreError::Forbidden(
                "Confirmed entries can be changed only by the operator. Propose the change with time_propose instead.".into(),
            ));
        }
        let next = apply_edit(&tx, &row, &input)?;
        let reason = optional_text("reason", input.reason.as_deref(), MAX_TEXT)?;
        let changed = write_edit(&tx, actor, &row, &next, reason.as_deref(), None)?;
        if !changed {
            let entry = with_warnings(&tx, row)?;
            return Ok(entry);
        }
        let entry = with_warnings(&tx, load_row(&tx, &input.id)?.ok_or(StoreError::NotFound)?)?;
        tx.commit().map_err(internal)?;
        Ok(entry)
    }

    pub fn delete(&self, actor: &Actor, input: DeleteEntry) -> Result<Entry, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let row = load_live(&tx, &input.id, input.version)?;
        if !actor.is_operator() && row.state == EntryState::Confirmed {
            return Err(StoreError::Forbidden(
                "Confirmed entries can be deleted only by the operator.".into(),
            ));
        }
        let reason = optional_text("reason", input.reason.as_deref(), MAX_TEXT)?;
        tx.execute(
            "UPDATE entry SET deleted_at=?1,version=version+1,updated_at=?1 WHERE id=?2",
            params![time::utc_now(), input.id],
        )
        .map_err(internal)?;
        let version = row.version + 1;
        revise(
            &tx,
            &input.id,
            version,
            actor,
            "delete",
            &json!({"deleted": false}),
            &json!({"deleted": true}),
            reason.as_deref(),
            None,
        )?;
        let entry = load_row(&tx, &input.id)?
            .ok_or(StoreError::NotFound)?
            .into_entry(vec![], 0);
        tx.commit().map_err(internal)?;
        Ok(entry)
    }

    pub fn confirm(&self, actor: &Actor, input: ConfirmEntries) -> Result<Vec<Entry>, StoreError> {
        operator_only(actor, "Only the operator confirms entries.")?;
        if input.entries.is_empty() {
            return Err(invalid("nothing to confirm"));
        }
        if input.entries.len() > 500 {
            return Err(invalid("confirm at most 500 entries at once"));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let mut ids = Vec::with_capacity(input.entries.len());
        for item in &input.entries {
            let row = load_live(&tx, &item.id, item.version)?;
            if row.content.running() {
                return Err(invalid("Stop the timer before confirming it."));
            }
            if row.state == EntryState::Draft {
                tx.execute(
                    "UPDATE entry SET state='confirmed',version=version+1,updated_at=?1 WHERE id=?2",
                    params![time::utc_now(), item.id],
                )
                .map_err(internal)?;
                revise(
                    &tx,
                    &item.id,
                    row.version + 1,
                    actor,
                    "confirm",
                    &json!({"state": "draft"}),
                    &json!({"state": "confirmed"}),
                    None,
                    None,
                )?;
            }
            ids.push(item.id.clone());
        }
        let entries = ids
            .iter()
            .map(|id| with_warnings(&tx, load_row(&tx, id)?.ok_or(StoreError::NotFound)?))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(internal)?;
        Ok(entries)
    }

    pub fn start(&self, actor: &Actor, input: StartTimer) -> Result<Entry, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        if let Some(row) = running_row(&tx)? {
            return Err(StoreError::TimerRunning(row.id));
        }
        let (project_id, project_slug, billable_default) = active_project(&tx, &input.project)?;
        let start = match &input.started_at {
            Some(raw) => time::parse_instant(raw).map_err(invalid)?,
            None => time::now(),
        };
        let content = Content {
            project_id,
            project_slug,
            date: start.date_naive(),
            start: Some(start),
            end: None,
            minutes: None,
            description: input.description.trim().to_string(),
            billable: input.billable.unwrap_or(billable_default),
            refs: normalize_refs(&input.refs)?,
        };
        validate(&content)?;
        let state = initial_state(actor);
        let id = insert(&tx, actor, &content, state, None)?;
        revise(
            &tx,
            &id,
            1,
            actor,
            "start",
            &Value::Null,
            &content.snapshot(state, false),
            None,
            None,
        )?;
        let entry = with_warnings(&tx, load_row(&tx, &id)?.ok_or(StoreError::NotFound)?)?;
        tx.commit().map_err(internal)?;
        Ok(entry)
    }

    pub fn stop(&self, actor: &Actor, input: StopTimer) -> Result<Entry, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let row = running_row(&tx)?.ok_or_else(|| invalid("No timer is running."))?;
        if !actor.is_operator() && row.state == EntryState::Confirmed {
            return Err(StoreError::Forbidden(
                "The running timer is the operator's; stop it in the UI.".into(),
            ));
        }
        let before = row.content.clone();
        let mut next = before.clone();
        let end = match &input.ended_at {
            Some(raw) => time::parse_instant(raw).map_err(invalid)?,
            None => time::now(),
        };
        let start = before
            .start
            .ok_or_else(|| StoreError::Internal("running entry without start".into()))?;
        if end <= start {
            return Err(invalid("The end must be after the start."));
        }
        let minutes = time::span_minutes(&start, &end).max(1);
        if minutes > MAX_MINUTES {
            return Err(invalid(
                "The timer ran more than 24 hours; give `ended_at` for when you actually stopped.",
            ));
        }
        next.end = Some(end);
        next.minutes = Some(minutes);
        if let Some(description) = &input.description {
            next.description = description.trim().to_string();
        }
        validate(&next)?;
        let version = write_content(&tx, &row.id, &next)?;
        revise(
            &tx,
            &row.id,
            version,
            actor,
            "stop",
            &before.snapshot(row.state, false),
            &next.snapshot(row.state, false),
            None,
            None,
        )?;
        let entry = with_warnings(&tx, load_row(&tx, &row.id)?.ok_or(StoreError::NotFound)?)?;
        tx.commit().map_err(internal)?;
        Ok(entry)
    }

    // ---- Proposals --------------------------------------------------------

    /// An agent proposes a change to a confirmed entry. Checked now against the
    /// same rules as an operator edit; applied only when the operator accepts.
    pub fn propose(&self, actor: &Actor, input: ProposeChange) -> Result<Proposal, StoreError> {
        let Actor::Agent(_) = actor else {
            return Err(StoreError::Forbidden(
                "The operator edits entries directly; proposals are for agents.".into(),
            ));
        };
        let reason = required_text("reason", &input.reason, MAX_TEXT)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let row = load_live(&tx, &input.id, input.version)?;
        if row.state == EntryState::Draft {
            return Err(invalid(
                "This entry is a draft; change it directly with time_edit or time_delete.",
            ));
        }
        let author = actor.label();
        let pending: Option<String> = tx
            .query_row(
                "SELECT id FROM proposal WHERE entry_id=?1 AND author=?2 AND state='pending'",
                params![row.id, author],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        if let Some(id) = pending {
            return Err(StoreError::PendingProposal(id));
        }
        let edit = input.as_edit();
        let (action, changes) = if input.delete {
            if has_fields(&edit) {
                return Err(invalid("Propose a deletion or a change, not both."));
            }
            ("delete", json!({"deleted": [false, true]}))
        } else {
            let next = apply_edit(&tx, &row, &edit)?;
            let changes = diff(
                &row.content.snapshot(row.state, false),
                &next.snapshot(row.state, false),
            );
            if changes.as_object().is_some_and(|c| c.is_empty()) {
                return Err(invalid("Nothing would change."));
            }
            ("update", changes)
        };
        let id = Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO proposal(id,entry_id,entry_version,action,input_json,changes_json,reason,author,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9)",
            params![
                id,
                row.id,
                row.version,
                action,
                serde_json::to_string(&edit).map_err(internal)?,
                changes.to_string(),
                reason,
                author,
                time::utc_now()
            ],
        )
        .map_err(internal)?;
        let proposal = load_proposal(&tx, &id)?;
        tx.commit().map_err(internal)?;
        Ok(proposal)
    }

    pub fn proposals(&self, filter: &ListProposals) -> Result<Vec<Proposal>, StoreError> {
        let connection = self.lock()?;
        let ids = {
            let mut statement = connection
                .prepare("SELECT id FROM proposal WHERE (?1 IS NULL OR state=?1) AND (?2 IS NULL OR entry_id=?2) ORDER BY created_at DESC, id DESC LIMIT 500")
                .map_err(internal)?;
            statement
                .query_map(
                    params![filter.state.map(ProposalState::as_str), filter.entry],
                    |r| r.get::<_, String>(0),
                )
                .map_err(internal)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(internal)?
        };
        ids.iter()
            .map(|id| load_proposal(&connection, id))
            .collect()
    }

    /// The proposing agent takes back its pending proposal.
    pub fn withdraw(&self, actor: &Actor, input: WithdrawProposal) -> Result<Proposal, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let proposal = load_proposal(&tx, &input.id)?;
        if proposal.author != actor.label() {
            return Err(StoreError::Forbidden(
                "Only the agent that made a proposal can withdraw it; the operator rejects it."
                    .into(),
            ));
        }
        pending(&proposal)?;
        decide(&tx, &proposal.id, ProposalState::Withdrawn, actor, None)?;
        let proposal = load_proposal(&tx, &input.id)?;
        tx.commit().map_err(internal)?;
        Ok(proposal)
    }

    pub fn reject(&self, actor: &Actor, input: DecideProposal) -> Result<Proposal, StoreError> {
        operator_only(actor, "Only the operator rejects proposals.")?;
        let note = optional_text("note", input.note.as_deref(), MAX_TEXT)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let proposal = load_proposal(&tx, &input.id)?;
        pending(&proposal)?;
        decide(
            &tx,
            &proposal.id,
            ProposalState::Rejected,
            actor,
            note.as_deref(),
        )?;
        let proposal = load_proposal(&tx, &input.id)?;
        tx.commit().map_err(internal)?;
        Ok(proposal)
    }

    /// Applies a pending proposal as an operator edit, keeping the entry's
    /// state. Refused when the entry changed since the proposal was made.
    pub fn accept(&self, actor: &Actor, input: DecideProposal) -> Result<Proposal, StoreError> {
        operator_only(actor, "Only the operator accepts proposals.")?;
        let note = optional_text("note", input.note.as_deref(), MAX_TEXT)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let proposal = load_proposal(&tx, &input.id)?;
        pending(&proposal)?;
        if proposal.stale {
            return Err(StoreError::Stale(
                "The entry changed after this proposal was made. Reject it, and ask for a new one if it still applies.".into(),
            ));
        }
        let row = load_live(&tx, &proposal.entry_id, proposal.entry_version)?;
        let accepted = Accepted {
            id: &proposal.id,
            author: &proposal.author,
        };
        let reason = Some(proposal.reason.as_str());
        if proposal.action == "delete" {
            tx.execute(
                "UPDATE entry SET deleted_at=?1,version=version+1,updated_at=?1 WHERE id=?2",
                params![time::utc_now(), row.id],
            )
            .map_err(internal)?;
            revise(
                &tx,
                &row.id,
                row.version + 1,
                actor,
                "delete",
                &json!({"deleted": false}),
                &json!({"deleted": true}),
                reason,
                Some(&accepted),
            )?;
        } else {
            let raw: String = tx
                .query_row(
                    "SELECT input_json FROM proposal WHERE id=?1",
                    [&proposal.id],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            let edit: EditEntry = serde_json::from_str(&raw).map_err(internal)?;
            let next = apply_edit(&tx, &row, &edit)?;
            write_edit(&tx, actor, &row, &next, reason, Some(&accepted))?;
        }
        decide(
            &tx,
            &proposal.id,
            ProposalState::Accepted,
            actor,
            note.as_deref(),
        )?;
        let proposal = load_proposal(&tx, &input.id)?;
        tx.commit().map_err(internal)?;
        Ok(proposal)
    }
}

// ---- Helpers ---------------------------------------------------------------

/// The entry's content after `input`, checked. Shared by edits and proposals,
/// so a proposal is held to exactly the rules of an operator edit.
fn apply_edit(tx: &Transaction<'_>, row: &Row, input: &EditEntry) -> Result<Content, StoreError> {
    let before = row.content.clone();
    let mut next = row.content.clone();
    let running = before.running();
    if let Some(slug) = &input.project {
        let (id, slug, _) = active_project(tx, slug)?;
        next.project_id = id;
        next.project_slug = slug;
    }
    if running && (input.ended_at.is_some() || input.minutes.is_some() || input.clear_times) {
        return Err(invalid(
            "Stop the timer before changing its end, minutes or times.",
        ));
    }
    let mut times_changed = false;
    if input.clear_times {
        next.start = None;
        next.end = None;
        times_changed = true;
    }
    if let Some(raw) = &input.started_at {
        next.start = Some(time::parse_instant(raw).map_err(invalid)?);
        times_changed = true;
    }
    if let Some(raw) = &input.ended_at {
        next.end = Some(time::parse_instant(raw).map_err(invalid)?);
        times_changed = true;
    }
    match &input.date {
        Some(date) => next.date = time::parse_date(date).map_err(invalid)?,
        None => {
            if let (true, Some(start)) = (times_changed, next.start) {
                next.date = start.date_naive();
            }
        }
    }
    match input.minutes {
        Some(minutes) => next.minutes = Some(minutes),
        None => {
            if let (true, Some(start), Some(end)) =
                (times_changed && !running, next.start, next.end)
            {
                next.minutes = Some(time::span_minutes(&start, &end).max(1));
            }
        }
    }
    if let Some(description) = &input.description {
        next.description = description.trim().to_string();
    }
    if let Some(billable) = input.billable {
        next.billable = billable;
    }
    if let Some(refs) = &input.refs {
        next.refs = normalize_refs(refs)?;
    }
    validate(&next)?;
    Ok(next)
}

/// Writes an edit and its revision. False when nothing would change.
fn write_edit(
    tx: &Transaction<'_>,
    actor: &Actor,
    row: &Row,
    next: &Content,
    reason: Option<&str>,
    proposal: Option<&Accepted<'_>>,
) -> Result<bool, StoreError> {
    let before = row.content.snapshot(row.state, false);
    let after = next.snapshot(row.state, false);
    if before == after {
        return Ok(false);
    }
    let version = write_content(tx, &row.id, next)?;
    revise(
        tx, &row.id, version, actor, "update", &before, &after, reason, proposal,
    )?;
    Ok(true)
}

fn has_fields(edit: &EditEntry) -> bool {
    edit.project.is_some()
        || edit.date.is_some()
        || edit.started_at.is_some()
        || edit.ended_at.is_some()
        || edit.clear_times
        || edit.minutes.is_some()
        || edit.description.is_some()
        || edit.billable.is_some()
        || edit.refs.is_some()
}

fn pending(proposal: &Proposal) -> Result<(), StoreError> {
    if proposal.state == ProposalState::Pending {
        Ok(())
    } else {
        Err(invalid(format!(
            "This proposal is already {}.",
            proposal.state.as_str()
        )))
    }
}

fn decide(
    tx: &Transaction<'_>,
    id: &str,
    state: ProposalState,
    actor: &Actor,
    note: Option<&str>,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE proposal SET state=?1,decided_by=?2,decided_at=?3,note=?4 WHERE id=?5 AND state='pending'",
        params![state.as_str(), actor.label(), time::utc_now(), note, id],
    )
    .map_err(internal)?;
    Ok(())
}

fn load_proposal(connection: &Connection, id: &str) -> Result<Proposal, StoreError> {
    type Fields = (
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );
    let fields: Option<Fields> = connection
        .query_row(
            "SELECT id,entry_id,entry_version,action,changes_json,reason,author,state,note,decided_by,created_at,decided_at FROM proposal WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    let (
        id,
        entry_id,
        entry_version,
        action,
        changes,
        reason,
        author,
        state,
        note,
        decided_by,
        created_at,
        decided_at,
    ) = fields.ok_or(StoreError::NotFound)?;
    let state = ProposalState::parse(&state).unwrap_or(ProposalState::Pending);
    let row = load_row(connection, &entry_id)?.ok_or(StoreError::NotFound)?;
    let stale = state == ProposalState::Pending && (row.deleted || row.version != entry_version);
    Ok(Proposal {
        id,
        entry_id,
        entry_version,
        action,
        changes: serde_json::from_str(&changes).unwrap_or(Value::Null),
        reason,
        author,
        state,
        note,
        decided_by,
        created_at,
        decided_at,
        stale,
        entry: with_warnings(connection, row)?,
    })
}

fn initial_state(actor: &Actor) -> EntryState {
    if actor.is_operator() {
        EntryState::Confirmed
    } else {
        EntryState::Draft
    }
}

fn operator_only(actor: &Actor, message: &str) -> Result<(), StoreError> {
    if actor.is_operator() {
        Ok(())
    } else {
        Err(StoreError::Forbidden(message.into()))
    }
}

fn insert(
    tx: &Transaction<'_>,
    actor: &Actor,
    content: &Content,
    state: EntryState,
    import_key: Option<&str>,
) -> Result<String, StoreError> {
    let id = Uuid::now_v7().to_string();
    let now = time::utc_now();
    tx.execute(
        "INSERT INTO entry(id,project_id,date,started_at,ended_at,start_ms,end_ms,minutes,description,billable,state,author,refs_json,import_key,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)",
        params![
            id,
            content.project_id,
            content.date.to_string(),
            content.start.as_ref().map(time::format),
            content.end.as_ref().map(time::format),
            content.start_ms(),
            content.end_ms(),
            content.minutes,
            content.description,
            content.billable,
            state.as_str(),
            actor.label(),
            serde_json::to_string(&content.refs).map_err(internal)?,
            import_key,
            now,
        ],
    )
    .map_err(|err| match err {
        rusqlite::Error::SqliteFailure(ref e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            invalid("another timer is already running")
        }
        other => internal(other),
    })?;
    Ok(id)
}

/// Writes the content and bumps the version; returns the new version.
fn write_content(tx: &Transaction<'_>, id: &str, content: &Content) -> Result<i64, StoreError> {
    tx.execute(
        "UPDATE entry SET project_id=?1,date=?2,started_at=?3,ended_at=?4,start_ms=?5,end_ms=?6,minutes=?7,description=?8,billable=?9,refs_json=?10,version=version+1,updated_at=?11 WHERE id=?12",
        params![
            content.project_id,
            content.date.to_string(),
            content.start.as_ref().map(time::format),
            content.end.as_ref().map(time::format),
            content.start_ms(),
            content.end_ms(),
            content.minutes,
            content.description,
            content.billable,
            serde_json::to_string(&content.refs).map_err(internal)?,
            time::utc_now(),
            id,
        ],
    )
    .map_err(internal)?;
    tx.query_row("SELECT version FROM entry WHERE id=?1", [id], |r| r.get(0))
        .map_err(internal)
}

/// `{field: [before, after]}` for each field that differs.
fn diff(before: &Value, after: &Value) -> Value {
    let mut changes = serde_json::Map::new();
    if let Value::Object(after) = after {
        for (field, value) in after {
            let old = before.get(field).cloned().unwrap_or(Value::Null);
            if &old != value {
                changes.insert(field.clone(), json!([old, value]));
            }
        }
    }
    Value::Object(changes)
}

/// The proposal an operator revision applies.
struct Accepted<'a> {
    id: &'a str,
    author: &'a str,
}

#[allow(clippy::too_many_arguments)]
fn revise(
    tx: &Transaction<'_>,
    id: &str,
    version: i64,
    actor: &Actor,
    action: &str,
    before: &Value,
    after: &Value,
    reason: Option<&str>,
    proposal: Option<&Accepted<'_>>,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO revision(entry_id,version,actor,via,action,changes_json,reason,at,proposal_id,proposed_by) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            id,
            version,
            actor.label(),
            actor.via(),
            action,
            diff(before, after).to_string(),
            reason,
            time::utc_now(),
            proposal.map(|p| p.id),
            proposal.map(|p| p.author),
        ],
    )
    .map_err(internal)?;
    Ok(())
}

fn load_row(connection: &Connection, id: &str) -> Result<Option<Row>, StoreError> {
    connection
        .query_row(&format!("{ROW_SELECT} WHERE e.id=?1"), [id], row_from)
        .optional()
        .map_err(internal)
}

/// A live (not deleted) entry at the expected version.
fn load_live(connection: &Connection, id: &str, version: i64) -> Result<Row, StoreError> {
    let row = load_row(connection, id)?.ok_or(StoreError::NotFound)?;
    if row.deleted {
        return Err(StoreError::NotFound);
    }
    if row.version != version {
        return Err(StoreError::Conflict);
    }
    Ok(row)
}

fn running_row(connection: &Connection) -> Result<Option<Row>, StoreError> {
    connection
        .query_row(
            &format!("{ROW_SELECT} WHERE e.minutes IS NULL AND e.deleted_at IS NULL"),
            [],
            row_from,
        )
        .optional()
        .map_err(internal)
}

fn with_warnings(connection: &Connection, row: Row) -> Result<Entry, StoreError> {
    let mut warnings = Vec::new();
    let c = &row.content;
    let now_ms = time::now().timestamp_millis();
    if !row.deleted {
        match (c.start_ms(), c.minutes) {
            (Some(start), None) if now_ms - start > LONG_MINUTES * 60_000 => {
                warnings.push(Warning {
                    code: "long_timer",
                    message: "This timer has run more than 10 hours.".into(),
                    entry: None,
                })
            }
            (_, Some(minutes)) if minutes > LONG_MINUTES => warnings.push(Warning {
                code: "long_entry",
                message: "More than 10 hours in one entry.".into(),
                entry: None,
            }),
            _ => {}
        }
        match c.start_ms() {
            None => warnings.push(Warning {
                code: "untimed",
                message: "No start time, so overlaps can't be checked.".into(),
                entry: None,
            }),
            Some(start) => {
                let end = c.end_ms().unwrap_or(now_ms);
                let mut statement = connection
                    .prepare_cached("SELECT id FROM entry WHERE deleted_at IS NULL AND id<>?1 AND start_ms IS NOT NULL AND start_ms<?3 AND COALESCE(end_ms,?4)>?2 ORDER BY start_ms LIMIT 5")
                    .map_err(internal)?;
                let others = statement
                    .query_map(params![row.id, start, end, now_ms], |r| {
                        r.get::<_, String>(0)
                    })
                    .map_err(internal)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(internal)?;
                for other in others {
                    warnings.push(Warning {
                        code: "overlap",
                        message: "Overlaps another entry.".into(),
                        entry: Some(other),
                    });
                }
            }
        }
    }
    let pending: i64 = connection
        .prepare_cached("SELECT count(*) FROM proposal WHERE entry_id=?1 AND state='pending'")
        .and_then(|mut s| s.query_row([&row.id], |r| r.get(0)))
        .map_err(internal)?;
    Ok(row.into_entry(warnings, pending))
}

fn validate(c: &Content) -> Result<(), StoreError> {
    if c.description.is_empty() {
        return Err(invalid("`description` is required"));
    }
    if c.description.chars().count() > MAX_TEXT {
        return Err(invalid(format!(
            "`description` is longer than {MAX_TEXT} characters"
        )));
    }
    if let Some(minutes) = c.minutes
        && !(1..=MAX_MINUTES).contains(&minutes)
    {
        return Err(invalid("`minutes` must be 1 to 1440"));
    }
    if c.end.is_some() && c.start.is_none() {
        return Err(invalid("`ended_at` needs `started_at`"));
    }
    let now = time::now();
    let slack = chrono::Duration::minutes(5);
    if let Some(start) = c.start {
        if start > now + slack {
            return Err(invalid("`started_at` is in the future"));
        }
        if start.date_naive() != c.date {
            return Err(invalid(format!(
                "`date` {} doesn't match the start's local date {}",
                c.date,
                start.date_naive()
            )));
        }
    }
    if let (Some(start), Some(end)) = (c.start, c.end) {
        if end <= start {
            return Err(invalid("`ended_at` must be after `started_at`"));
        }
        if end > now + slack {
            return Err(invalid("`ended_at` is in the future"));
        }
        let span_seconds = (end - start).num_seconds();
        if span_seconds > MAX_MINUTES * 60 {
            return Err(invalid("an entry spans at most 24 hours"));
        }
        if let Some(minutes) = c.minutes
            && minutes * 60 > span_seconds + 59
        {
            return Err(invalid(
                "`minutes` is more than the time between start and end",
            ));
        }
    }
    if c.date > time::today() + chrono::Duration::days(1) {
        return Err(invalid("`date` is in the future"));
    }
    Ok(())
}

fn normalize_refs(refs: &[String]) -> Result<Vec<String>, StoreError> {
    if refs.len() > MAX_REFS {
        return Err(invalid(format!("at most {MAX_REFS} refs")));
    }
    let mut out: Vec<String> = Vec::with_capacity(refs.len());
    for raw in refs {
        let value = raw.trim();
        if !valid_ref(value) {
            return Err(invalid(format!(
                "`{value}` is not a ref; use kind:value with a kind of {}, or an http(s) URL",
                REF_KINDS.join(", ")
            )));
        }
        if !out.iter().any(|existing| existing == value) {
            out.push(value.to_string());
        }
    }
    Ok(out)
}

pub fn valid_ref(value: &str) -> bool {
    if value.is_empty() || value.len() > 300 || value.chars().any(char::is_whitespace) {
        return false;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return url::Url::parse(value).is_ok_and(|u| u.host_str().is_some());
    }
    let Some((kind, rest)) = value.split_once(':') else {
        return false;
    };
    REF_KINDS.contains(&kind)
        && !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/#@:-".contains(c))
}

fn validate_slug(slug: &str) -> Result<(), StoreError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "a slug is lowercase letters and digits in words joined by dashes",
        ))
    }
}

fn required_text(field: &str, value: &str, max: usize) -> Result<String, StoreError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(format!("`{field}` is required")));
    }
    if value.chars().count() > max {
        return Err(invalid(format!(
            "`{field}` is longer than {max} characters"
        )));
    }
    Ok(value.to_string())
}

fn optional_text(
    field: &str,
    value: Option<&str>,
    max: usize,
) -> Result<Option<String>, StoreError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => required_text(field, value, max).map(Some),
    }
}

fn period(date: NaiveDate, by: Grouping) -> String {
    match by {
        Grouping::Day => date.to_string(),
        Grouping::Week => {
            let week = date.iso_week();
            format!("{}-W{:02}", week.year(), week.week())
        }
        Grouping::Month => format!("{}-{:02}", date.year(), date.month()),
    }
}

fn read_projects(connection: &Connection) -> Result<Vec<Project>, StoreError> {
    let mut statement = connection
        .prepare("SELECT slug,name,client,billable_default,archived,version,created_at,updated_at FROM project ORDER BY archived, name COLLATE NOCASE")
        .map_err(internal)?;
    statement
        .query_map([], project_from)
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)
}

fn read_project(connection: &Connection, slug: &str) -> Result<Option<Project>, StoreError> {
    connection
        .query_row(
            "SELECT slug,name,client,billable_default,archived,version,created_at,updated_at FROM project WHERE slug=?1",
            [slug],
            project_from,
        )
        .optional()
        .map_err(internal)
}

fn project_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        slug: r.get(0)?,
        name: r.get(1)?,
        client: r.get(2)?,
        billable_default: r.get(3)?,
        archived: r.get(4)?,
        version: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn project_id(connection: &Connection, slug: &str) -> Result<String, StoreError> {
    connection
        .query_row("SELECT id FROM project WHERE slug=?1", [slug.trim()], |r| {
            r.get(0)
        })
        .optional()
        .map_err(internal)?
        .ok_or_else(|| invalid(format!("no project `{}`", slug.trim())))
}

/// (id, slug, billable_default) of a project that takes new time.
fn active_project(
    connection: &Connection,
    slug: &str,
) -> Result<(String, String, bool), StoreError> {
    let slug = slug.trim();
    let found: Option<(String, bool, bool)> = connection
        .query_row(
            "SELECT id,billable_default,archived FROM project WHERE slug=?1",
            [slug],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(internal)?;
    match found {
        None => Err(invalid(format!(
            "no project `{slug}`; the operator adds projects"
        ))),
        Some((_, _, true)) => Err(invalid(format!("project `{slug}` is archived"))),
        Some((id, billable, false)) => Ok((id, slug.to_string(), billable)),
    }
}

fn migrate(connection: &mut Connection) -> anyhow::Result<()> {
    let current: usize =
        connection.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))? as usize;
    if current > MIGRATIONS.len() {
        bail!(
            "this database is from a newer hora (schema {current}, this build knows {})",
            MIGRATIONS.len()
        );
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = connection.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("restricting {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> anyhow::Result<()> {
    Ok(())
}
