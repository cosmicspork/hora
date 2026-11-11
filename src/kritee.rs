//! Turns a kritee export (`projects.jsonl`, `time_entries.jsonl`) into
//! `time_log` inputs. The import runs through the agent API like any agent,
//! so every entry arrives as a draft for the operator to confirm.
//!
//! kritee stored naive times. Its app timezone was UTC, so a timer entry
//! (started when it was created) holds UTC; a manually typed entry holds the
//! wall-clock time where the user was, which changed over the year. `--zone`
//! ranges say where that was.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::time;

#[derive(Deserialize)]
struct KProject {
    id: i64,
    slug: String,
}

#[derive(Deserialize)]
struct KEntry {
    id: i64,
    project_id: Option<i64>,
    description: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    duration_minutes: Option<i64>,
    is_billable: Option<bool>,
    created_at: String,
}

/// Which timezone typed times were in, from a date onward.
#[derive(Clone, Debug)]
pub struct Zones {
    default: Tz,
    from: Vec<(NaiveDate, Tz)>,
}

impl Zones {
    pub fn new(default: Tz, mut from: Vec<(NaiveDate, Tz)>) -> Self {
        from.sort_by_key(|(date, _)| *date);
        Self { default, from }
    }

    /// `2026-06-20=Europe/Berlin`.
    pub fn parse_range(raw: &str) -> Result<(NaiveDate, Tz)> {
        let (date, zone) = raw
            .split_once('=')
            .context("a zone range is DATE=Area/City, such as 2026-06-20=Europe/Berlin")?;
        let date =
            NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").context("invalid zone date")?;
        let zone: Tz = zone.trim().parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok((date, zone))
    }

    pub fn on(&self, date: NaiveDate) -> Tz {
        self.from
            .iter()
            .rev()
            .find(|(from, _)| *from <= date)
            .map(|(_, zone)| *zone)
            .unwrap_or(self.default)
    }
}

pub struct Planned {
    pub kritee_id: i64,
    pub zone: String,
    pub input: Value,
}

fn naive(raw: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .with_context(|| format!("unreadable kritee time `{raw}`"))
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{} line {}", path.display(), index + 1))
        })
        .collect()
}

pub fn plan(dir: &Path, zones: &Zones) -> Result<Vec<Planned>> {
    let projects: HashMap<i64, String> = read_jsonl::<KProject>(&dir.join("projects.jsonl"))?
        .into_iter()
        .map(|p| (p.id, p.slug))
        .collect();
    let entries: Vec<KEntry> = read_jsonl(&dir.join("time_entries.jsonl"))?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let Some(slug) = e.project_id.and_then(|id| projects.get(&id)) else {
            bail!("kritee time entry {} has no known project", e.id);
        };
        let created = Utc.from_utc_datetime(&naive(&e.created_at)?);
        let key = format!("kritee:time_entry:{}", e.id);
        let mut input = json!({
            "project": slug,
            "description": e.description.as_deref().unwrap_or("").trim(),
            "billable": e.is_billable.unwrap_or(true),
            "refs": [key],
            "import_key": key,
        });
        let zone_label;
        match &e.started_at {
            Some(started) => {
                // A timer entry was started the moment it was created, in UTC.
                let timer = started == &e.created_at;
                let to_instant = |raw: &str| -> Result<DateTime<FixedOffset>> {
                    let n = naive(raw)?;
                    if timer {
                        let utc = Utc.from_utc_datetime(&n);
                        Ok(utc
                            .with_timezone(&zones.on(utc.date_naive()))
                            .fixed_offset())
                    } else {
                        time::parse_instant_in(
                            &n.format("%Y-%m-%d %H:%M:%S").to_string(),
                            &zones.on(n.date()),
                        )
                        .map_err(anyhow::Error::msg)
                    }
                };
                let start = to_instant(started)?;
                input["started_at"] = json!(time::format(&start));
                if let Some(ended) = &e.ended_at {
                    input["ended_at"] = json!(time::format(&to_instant(ended)?));
                }
                zone_label = if timer {
                    format!("timer, UTC shown in {}", zones.on(start.date_naive()))
                } else {
                    zones.on(start.date_naive()).to_string()
                };
                input["reason"] = json!(format!(
                    "Imported from kritee time entry {} ({}).",
                    e.id,
                    if timer {
                        "a timer entry, stored in UTC"
                    } else {
                        "typed times read as local"
                    }
                ));
            }
            None => {
                let zone = zones.on(created.date_naive());
                let date = created.with_timezone(&zone).date_naive();
                input["date"] = json!(date.to_string());
                zone_label = format!("date only, from entry time in {zone}");
                input["reason"] = json!(format!(
                    "Imported from kritee time entry {}. kritee had no start time; the date is when it was entered.",
                    e.id
                ));
            }
        }
        if let Some(minutes) = e.duration_minutes {
            input["minutes"] = json!(minutes);
        }
        out.push(Planned {
            kritee_id: e.id,
            zone: zone_label,
            input,
        });
    }
    Ok(out)
}
