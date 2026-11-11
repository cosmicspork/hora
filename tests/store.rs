use chrono::Duration;
use hora::model::{
    Actor, ConfirmEntries, CreateProject, DecideProposal, DeleteEntry, EditEntry, EntryState,
    EntryVersion, Grouping, ListEntries, ListProposals, LogEntry, ProposalState, ProposeChange,
    StartTimer, StopTimer, SummaryQuery, WithdrawProposal,
};
use hora::store::{Store, StoreError};
use hora::time;

fn agent() -> Actor {
    Actor::Agent("claude-code".into())
}

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .create_project(
            &Actor::Operator,
            CreateProject {
                slug: "acme".into(),
                name: "Acme".into(),
                client: Some("Acme".into()),
                billable_default: Some(true),
            },
        )
        .unwrap();
    (dir, store)
}

/// Yesterday at `hh:mm` local time, as a naive string the server reads as local.
fn yesterday(hm: &str) -> String {
    format!("{} {hm}", time::today() - Duration::days(1))
}

fn log(minutes: i64) -> LogEntry {
    LogEntry {
        project: "acme".into(),
        minutes: Some(minutes),
        description: "bugfixes".into(),
        ..Default::default()
    }
}

#[test]
fn agents_create_drafts_and_the_operator_creates_confirmed_entries() {
    let (_dir, store) = setup();
    let draft = store.log(&agent(), log(30)).unwrap();
    assert_eq!(draft.state, EntryState::Draft);
    assert_eq!(draft.author, "agent:claude-code");
    assert!(draft.billable, "project default applies");
    let confirmed = store.log(&Actor::Operator, log(45)).unwrap();
    assert_eq!(confirmed.state, EntryState::Confirmed);
    assert_eq!(confirmed.author, "operator");
    let history = store.history(&draft.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].action, "create");
    assert_eq!(history[0].via, "agent");
    assert_eq!(history[0].changes["minutes"], serde_json::json!([null, 30]));
}

#[test]
fn agents_change_drafts_but_never_confirmed_entries() {
    let (_dir, store) = setup();
    let draft = store.log(&agent(), log(30)).unwrap();
    let edited = store
        .edit(
            &agent(),
            EditEntry {
                id: draft.id.clone(),
                version: draft.version,
                minutes: Some(40),
                reason: Some("commit times say 40".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(edited.minutes, Some(40));
    assert_eq!(edited.version, 2);
    let history = store.history(&draft.id).unwrap();
    assert_eq!(history[1].changes, serde_json::json!({"minutes": [30, 40]}));
    assert_eq!(history[1].reason.as_deref(), Some("commit times say 40"));

    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    let refused = store.edit(
        &agent(),
        EditEntry {
            id: confirmed.id.clone(),
            version: confirmed.version,
            minutes: Some(90),
            ..Default::default()
        },
    );
    assert!(matches!(refused, Err(StoreError::Forbidden(_))));
    let refused = store.delete(
        &agent(),
        DeleteEntry {
            id: confirmed.id.clone(),
            version: confirmed.version,
            reason: None,
        },
    );
    assert!(matches!(refused, Err(StoreError::Forbidden(_))));
}

#[test]
fn only_the_operator_confirms_and_stale_versions_are_refused() {
    let (_dir, store) = setup();
    let draft = store.log(&agent(), log(30)).unwrap();
    let request = || ConfirmEntries {
        entries: vec![EntryVersion {
            id: draft.id.clone(),
            version: draft.version,
        }],
    };
    assert!(matches!(
        store.confirm(&agent(), request()),
        Err(StoreError::Forbidden(_))
    ));
    let confirmed = store.confirm(&Actor::Operator, request()).unwrap();
    assert_eq!(confirmed[0].state, EntryState::Confirmed);
    assert_eq!(confirmed[0].version, 2);
    assert!(matches!(
        store.confirm(&Actor::Operator, request()),
        Err(StoreError::Conflict)
    ));
    let history = store.history(&draft.id).unwrap();
    assert_eq!(history.last().unwrap().actor, "operator");
    assert_eq!(history.last().unwrap().action, "confirm");
}

#[test]
fn deleted_entries_leave_lists_but_keep_history() {
    let (_dir, store) = setup();
    let draft = store.log(&agent(), log(30)).unwrap();
    store
        .delete(
            &agent(),
            DeleteEntry {
                id: draft.id.clone(),
                version: 1,
                reason: Some("duplicate".into()),
            },
        )
        .unwrap();
    assert!(store.entries(&ListEntries::default()).unwrap().is_empty());
    assert!(store.entry(&draft.id).unwrap().deleted);
    assert_eq!(store.history(&draft.id).unwrap().len(), 2);
}

#[test]
fn one_timer_runs_and_agents_cannot_stop_the_operators() {
    let (_dir, store) = setup();
    let started_at = time::format(&(time::now() - Duration::minutes(50)));
    let timer = store
        .start(
            &Actor::Operator,
            StartTimer {
                project: "acme".into(),
                description: "client call".into(),
                started_at: Some(started_at),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(timer.running);
    let second = store.start(
        &agent(),
        StartTimer {
            project: "acme".into(),
            description: "other".into(),
            ..Default::default()
        },
    );
    assert!(matches!(second, Err(StoreError::TimerRunning(id)) if id == timer.id));
    assert!(matches!(
        store.stop(&agent(), StopTimer::default()),
        Err(StoreError::Forbidden(_))
    ));
    let stopped = store.stop(&Actor::Operator, StopTimer::default()).unwrap();
    assert!(!stopped.running);
    assert_eq!(stopped.minutes, Some(50));
    assert!(store.running().unwrap().is_none());
}

#[test]
fn import_keys_make_logging_idempotent() {
    let (_dir, store) = setup();
    let mut input = log(30);
    input.import_key = Some("kritee:time_entry:1".into());
    let first = store.log(&agent(), input.clone()).unwrap();
    assert!(matches!(store.log(&agent(), input), Err(StoreError::Duplicate(id)) if id == first.id));
}

#[test]
fn times_are_checked() {
    let (_dir, store) = setup();
    let spanned = store
        .log(
            &agent(),
            LogEntry {
                started_at: Some(yesterday("17:00")),
                ended_at: Some(yesterday("18:30")),
                minutes: None,
                ..log(1)
            },
        )
        .unwrap();
    assert_eq!(spanned.minutes, Some(90), "minutes default to the span");
    assert_eq!(
        spanned.date,
        (time::today() - Duration::days(1)).to_string()
    );

    let cases = [
        LogEntry {
            started_at: Some(yesterday("17:00")),
            ended_at: Some(yesterday("18:00")),
            minutes: Some(61),
            ..log(1)
        },
        LogEntry {
            ended_at: Some(yesterday("18:00")),
            ..log(30)
        },
        LogEntry {
            date: Some(time::today().to_string()),
            started_at: Some(yesterday("17:00")),
            ..log(30)
        },
        LogEntry {
            project: "nope".into(),
            ..log(30)
        },
        LogEntry {
            refs: vec!["not a ref".into()],
            ..log(30)
        },
        LogEntry {
            minutes: None,
            ..log(30)
        },
        LogEntry {
            minutes: Some(0),
            ..log(30)
        },
        LogEntry {
            started_at: Some(format!("{} 23:00", time::today() + Duration::days(1))),
            minutes: Some(30),
            ..log(30)
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        assert!(
            matches!(store.log(&agent(), case), Err(StoreError::Invalid(_))),
            "case {index} should be refused"
        );
    }
}

#[test]
fn refs_accept_known_kinds_and_urls() {
    let (_dir, store) = setup();
    let entry = store
        .log(
            &agent(),
            LogEntry {
                refs: vec![
                    "workboard:7f3c".into(),
                    "pr:acme-web#104".into(),
                    "commit:acme-web@abc1234".into(),
                    "https://github.com/acme/web/pull/104".into(),
                    "workboard:7f3c".into(),
                ],
                ..log(30)
            },
        )
        .unwrap();
    assert_eq!(entry.refs.len(), 4, "duplicates are dropped");
}

#[test]
fn overlapping_entries_are_flagged() {
    let (_dir, store) = setup();
    let first = store
        .log(
            &agent(),
            LogEntry {
                started_at: Some(yesterday("10:00")),
                ended_at: Some(yesterday("11:00")),
                minutes: None,
                ..log(1)
            },
        )
        .unwrap();
    let second = store
        .log(
            &agent(),
            LogEntry {
                started_at: Some(yesterday("10:30")),
                minutes: Some(60),
                ..log(1)
            },
        )
        .unwrap();
    assert!(
        second
            .warnings
            .iter()
            .any(|w| w.code == "overlap" && w.entry.as_deref() == Some(first.id.as_str()))
    );
    let untimed = store.log(&agent(), log(30)).unwrap();
    assert!(untimed.warnings.iter().any(|w| w.code == "untimed"));
}

#[test]
fn summary_splits_confirmed_billable_nonbillable_and_drafts() {
    let (_dir, store) = setup();
    store.log(&Actor::Operator, log(60)).unwrap();
    store
        .log(
            &Actor::Operator,
            LogEntry {
                billable: Some(false),
                ..log(30)
            },
        )
        .unwrap();
    store.log(&agent(), log(15)).unwrap();
    let today = time::today().to_string();
    let summary = store
        .summary(&SummaryQuery {
            from: today.clone(),
            to: today,
            project: Some("acme".into()),
            by: Grouping::Week,
        })
        .unwrap();
    assert_eq!(summary.rows.len(), 1);
    assert_eq!(summary.total.billable_minutes, 60);
    assert_eq!(summary.total.nonbillable_minutes, 30);
    assert_eq!(summary.total.draft_minutes, 15);
    assert_eq!(summary.total.entries, 3);
    assert!(summary.rows[0].period.contains("-W"));
}

#[test]
fn projects_are_the_operators() {
    let (_dir, store) = setup();
    let refused = store.create_project(
        &agent(),
        CreateProject {
            slug: "side-gig".into(),
            name: "Side gig".into(),
            client: None,
            billable_default: None,
        },
    );
    assert!(matches!(refused, Err(StoreError::Forbidden(_))));
    let bad_slug = store.create_project(
        &Actor::Operator,
        CreateProject {
            slug: "Side Gig".into(),
            name: "Side gig".into(),
            client: None,
            billable_default: None,
        },
    );
    assert!(matches!(bad_slug, Err(StoreError::Invalid(_))));
}

#[test]
fn reopening_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(dir.path()).unwrap();
        store
            .create_project(
                &Actor::Operator,
                CreateProject {
                    slug: "a".into(),
                    name: "A".into(),
                    client: None,
                    billable_default: None,
                },
            )
            .unwrap();
    }
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.projects().unwrap().len(), 1);
}

fn other_agent() -> Actor {
    Actor::Agent("codex".into())
}

fn propose_minutes(id: &str, version: i64, minutes: i64) -> ProposeChange {
    ProposeChange {
        id: id.into(),
        version,
        minutes: Some(minutes),
        reason: "the session log ends at 45 minutes".into(),
        ..Default::default()
    }
}

fn decide(id: &str) -> DecideProposal {
    DecideProposal {
        id: id.into(),
        note: None,
    }
}

#[test]
fn accepting_a_proposal_applies_it_as_the_operator_and_records_who_proposed_it() {
    let (_dir, store) = setup();
    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    let refused = store.edit(
        &agent(),
        EditEntry {
            id: confirmed.id.clone(),
            version: confirmed.version,
            minutes: Some(45),
            ..Default::default()
        },
    );
    assert!(
        matches!(refused, Err(StoreError::Forbidden(_))),
        "the gate stays"
    );

    let proposal = store
        .propose(
            &agent(),
            propose_minutes(&confirmed.id, confirmed.version, 45),
        )
        .unwrap();
    assert_eq!(proposal.state, ProposalState::Pending);
    assert_eq!(proposal.action, "update");
    assert_eq!(proposal.changes, serde_json::json!({"minutes": [60, 45]}));
    assert!(!proposal.stale);
    let unchanged = store.entry(&confirmed.id).unwrap();
    assert_eq!(unchanged.minutes, Some(60), "proposing changes nothing");
    assert_eq!(unchanged.pending_proposals, 1);
    assert_eq!(store.state().unwrap().proposals, 1);

    assert!(matches!(
        store.accept(&agent(), decide(&proposal.id)),
        Err(StoreError::Forbidden(_))
    ));
    let accepted = store
        .accept(
            &Actor::Operator,
            DecideProposal {
                id: proposal.id.clone(),
                note: Some("agreed".into()),
            },
        )
        .unwrap();
    assert_eq!(accepted.state, ProposalState::Accepted);
    assert_eq!(accepted.decided_by.as_deref(), Some("operator"));
    assert_eq!(accepted.note.as_deref(), Some("agreed"));
    assert_eq!(accepted.entry.minutes, Some(45));
    assert_eq!(accepted.entry.state, EntryState::Confirmed, "state is kept");
    assert_eq!(accepted.entry.pending_proposals, 0);

    let history = store.history(&confirmed.id).unwrap();
    let last = history.last().unwrap();
    assert_eq!(last.action, "update");
    assert_eq!(last.actor, "operator");
    assert_eq!(last.proposed_by.as_deref(), Some("agent:claude-code"));
    assert_eq!(last.proposal_id.as_deref(), Some(proposal.id.as_str()));
    assert_eq!(
        last.reason.as_deref(),
        Some("the session log ends at 45 minutes")
    );
    assert!(
        matches!(
            store.reject(&Actor::Operator, decide(&proposal.id)),
            Err(StoreError::Invalid(_))
        ),
        "decided proposals stay decided"
    );
}

#[test]
fn a_proposal_goes_stale_when_the_entry_changes() {
    let (_dir, store) = setup();
    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    let first = store
        .propose(&agent(), propose_minutes(&confirmed.id, 1, 45))
        .unwrap();
    let second = store
        .propose(&other_agent(), propose_minutes(&confirmed.id, 1, 50))
        .unwrap();
    store.accept(&Actor::Operator, decide(&first.id)).unwrap();
    let listed = store
        .proposals(&ListProposals {
            state: Some(ProposalState::Pending),
            entry: Some(confirmed.id.clone()),
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].stale);
    assert!(matches!(
        store.accept(&Actor::Operator, decide(&second.id)),
        Err(StoreError::Stale(_))
    ));
    assert_eq!(store.entry(&confirmed.id).unwrap().minutes, Some(45));
    let rejected = store.reject(&Actor::Operator, decide(&second.id)).unwrap();
    assert_eq!(rejected.state, ProposalState::Rejected);
    assert!(!rejected.stale, "only pending proposals are stale");
}

#[test]
fn one_pending_proposal_per_entry_per_agent_and_only_its_author_withdraws_it() {
    let (_dir, store) = setup();
    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    let proposal = store
        .propose(&agent(), propose_minutes(&confirmed.id, 1, 45))
        .unwrap();
    assert!(matches!(
        store.propose(&agent(), propose_minutes(&confirmed.id, 1, 40)),
        Err(StoreError::PendingProposal(id)) if id == proposal.id
    ));
    let withdraw = || WithdrawProposal {
        id: proposal.id.clone(),
    };
    assert!(matches!(
        store.withdraw(&other_agent(), withdraw()),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.withdraw(&Actor::Operator, withdraw()),
        Err(StoreError::Forbidden(_))
    ));
    let withdrawn = store.withdraw(&agent(), withdraw()).unwrap();
    assert_eq!(withdrawn.state, ProposalState::Withdrawn);
    assert!(matches!(
        store.accept(&Actor::Operator, decide(&proposal.id)),
        Err(StoreError::Invalid(_))
    ));
    store
        .propose(&agent(), propose_minutes(&confirmed.id, 1, 40))
        .expect("a new proposal after withdrawing");
}

#[test]
fn proposals_are_checked_like_operator_edits() {
    let (_dir, store) = setup();
    let draft = store.log(&agent(), log(30)).unwrap();
    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    assert!(
        matches!(
            store.propose(&agent(), propose_minutes(&draft.id, 1, 20)),
            Err(StoreError::Invalid(_))
        ),
        "drafts are edited directly"
    );
    assert!(matches!(
        store.propose(&Actor::Operator, propose_minutes(&confirmed.id, 1, 20)),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.propose(&agent(), propose_minutes(&confirmed.id, 2, 20)),
        Err(StoreError::Conflict)
    ));
    let cases = [
        ProposeChange {
            reason: " ".into(),
            ..propose_minutes(&confirmed.id, 1, 20)
        },
        propose_minutes(&confirmed.id, 1, 0),
        propose_minutes(&confirmed.id, 1, 60),
        ProposeChange {
            delete: true,
            ..propose_minutes(&confirmed.id, 1, 20)
        },
        ProposeChange {
            refs: Some(vec!["not a ref".into()]),
            minutes: None,
            ..propose_minutes(&confirmed.id, 1, 20)
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        assert!(
            matches!(store.propose(&agent(), case), Err(StoreError::Invalid(_))),
            "case {index} should be refused"
        );
    }
}

#[test]
fn an_accepted_deletion_deletes_the_entry() {
    let (_dir, store) = setup();
    let confirmed = store.log(&Actor::Operator, log(60)).unwrap();
    let proposal = store
        .propose(
            &agent(),
            ProposeChange {
                id: confirmed.id.clone(),
                version: 1,
                delete: true,
                reason: "duplicate of the Nov 10 entry".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(proposal.action, "delete");
    store
        .accept(&Actor::Operator, decide(&proposal.id))
        .unwrap();
    assert!(store.entry(&confirmed.id).unwrap().deleted);
    let last = store.history(&confirmed.id).unwrap().pop().unwrap();
    assert_eq!(last.action, "delete");
    assert_eq!(last.proposed_by.as_deref(), Some("agent:claude-code"));
}

#[test]
fn a_version_1_database_is_upgraded_in_place() {
    let dir = tempfile::tempdir().unwrap();
    {
        let connection = rusqlite::Connection::open(dir.path().join("hora.sqlite3")).unwrap();
        connection
            .execute_batch(hora::store::MIGRATIONS[0])
            .unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version=1;
                 INSERT INTO project(id,slug,name,billable_default,created_at,updated_at) VALUES('p','acme','Acme',1,'2026-11-01T00:00:00Z','2026-11-01T00:00:00Z');
                 INSERT INTO entry(id,project_id,date,minutes,description,billable,state,author,created_at,updated_at) VALUES('e','p','2026-11-01',60,'Fixes',1,'confirmed','operator','2026-11-01T00:00:00Z','2026-11-01T00:00:00Z');
                 INSERT INTO revision(entry_id,version,actor,via,action,changes_json,at) VALUES('e',1,'operator','ui','create','{}','2026-11-01T00:00:00Z');",
            )
            .unwrap();
    }
    let store = Store::open(dir.path()).unwrap();
    let version: i64 = rusqlite::Connection::open(dir.path().join("hora.sqlite3"))
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let history = store.history("e").unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].proposed_by.is_none());
    let proposal = store
        .propose(&agent(), propose_minutes("e", 1, 45))
        .unwrap();
    store
        .accept(&Actor::Operator, decide(&proposal.id))
        .unwrap();
    assert_eq!(store.entry("e").unwrap().minutes, Some(45));
}
