# hora

Local time tracking for client hours. Agents propose entries; you confirm them. Every change records who made it.

hora is one binary: a SQLite store, a web UI for you, and a CLI and HTTP API for agents. It listens on loopback only and keeps no credentials. It is built to become a [Tracon](https://github.com/cosmicspork/tracon) plugin later; until then it runs on its own.

## Install and run

Requires Rust and Bun to build.

```sh
bun install --cwd spa
bun run --cwd spa build
cargo install --path . --locked
hora serve
```

Open `http://localhost:4530/`. Build the frontend before compiling Rust: release builds embed `spa/dist`, and a missing `spa/dist` fails compilation.

`hora serve --port` changes the port (default 4530). `--data-dir` or `HORA_DATA_DIR` moves the database from `~/.local/share/hora/hora.sqlite3`. The directory and database are private (0700/0600). Back up the database and its WAL sidecars together while the server is stopped. A database from a newer hora is refused rather than downgraded.

To start hora on demand, install the units in `contrib/systemd/` into `~/.config/systemd/user/` and run `systemctl --user enable --now hora.socket`. hora takes the listening socket from systemd and binds its own port only when started by hand.

## Using it

- **Week** shows each day's entries with totals: confirmed billable, confirmed other, and drafts. Add time to a day, or click an entry to edit it and see its history.
- **The timer bar** starts and stops a timer. One runs at a time. A timer running over 10 hours is flagged; open it to correct the start, or stop it and fix the end.
- **Drafts** lists what agents proposed, oldest first. Check each against its refs, edit what's off, and confirm them singly or in bulk. Above the drafts are agents' proposed changes to confirmed entries: each shows the fields before and after, with the agent's reason. Accept applies it as your edit; Reject leaves the entry alone. Either can carry a note for the agent. An entry with a pending proposal is marked in Week.
- **Summary** totals hours by day, week or month and project, and exports CSV or JSONL.
- **Projects** holds the slugs agents log against. A slug never changes, so other tools can key on it.

Times keep the offset they were recorded in, so an entry worked in New York still reads 20:30 after you fly to Berlin. A recorded offset that differs from the browser's is shown next to the time.

Overlapping entries, entries over 10 hours, and entries with no start time get warnings. Nothing is refused for them.

## For agents

Set `HORA_AGENT` to your name; it is recorded as the author of every change. `HORA_URL` points at the server (default `http://127.0.0.1:4530`). Every command prints JSON.

```sh
export HORA_AGENT=claude-code
hora projects
hora log -p acme --start "2026-11-07 17:40" --end "2026-11-07 19:05" \
  -m "Vite 8 upgrade and CI fixes" --ref pr:acme-web#116 --ref commit:acme-web@4f2a9c1
hora log -p acme --date 2026-11-06 --minutes 45 -m "Client call" --reason "From calendar"
hora list --from 2026-11-01 --state draft
hora edit <id> --version 1 --minutes 50 --reason "Commits end at 18:30"
hora start -p acme -m "Content freshness fix"
hora stop
hora summary --from 2026-11-01 --to 2026-11-30 --by week
hora propose <id> --version 2 -m "Login fixes" --ref pr:acme-web#157 \
  --reason "Matches the session log"
hora proposals --state pending
hora withdraw <proposal-id>
```

What agents can and can't do:

- An agent's entry is a **draft** until you confirm it. Agents may edit and delete drafts, never confirmed entries.
- To change or delete a confirmed entry, an agent **proposes** it (`hora propose`, with `--delete` for a deletion). A proposal needs a reason and is checked against the same rules as your own edit, but changes nothing until you accept it. An agent has at most one pending proposal per entry, and may withdraw it. If the entry changes before you decide, the proposal goes stale and can only be rejected. Proposals on drafts are refused: edit the draft instead.
- Only you confirm entries, accept or reject proposals, and manage projects.
- An agent may stop only a timer an agent started.
- Times are RFC 3339 with an offset, or `YYYY-MM-DD HH:MM` in the server's local time. Give `minutes`, or a start and an end; minutes may be less than the span (breaks), never more.
- Cite evidence in refs: `workboard:<id>`, `jira:<KEY>`, `pr:<repo>#<n>`, `commit:<repo>@<sha>`, `kritee:<id>`, `tracon:<id>`, `issue:<...>`, or an http(s) URL.
- An `--import-key` makes logging idempotent: a second entry with the same key is refused as a duplicate, so a backfill can be re-run.

`hora ops` lists every operation with its JSON input schema, and `hora call <op> '<json>'` (or `-` for stdin) runs any of them. Over HTTP, `POST /api/agent/ops/<op>` with an `X-Hora-Agent` header and a JSON body does the same; `GET /api/agent/ops` lists them. The agent API refuses any request carrying an `Origin` header, so a web page can't call it.

## What the gate is, and isn't

hora keeps a cooperative agent accountable: its work arrives as drafts or proposals, and every change is recorded with its author. A change made by accepting a proposal is recorded as yours, with the proposing agent beside it. It does not stop a hostile one. An agent running as you can open the database, or send a request that claims to come from the web UI. The author recorded for agent changes is the name the agent gives. In the Tracon plugin, Tracon attests the caller instead.

## Data

The database is the source of truth; exports are copies. Other tools (an invoicing app) may read it while hora runs; they should not write it. Its schema, `PRAGMA user_version` 2 (a version 1 database is upgraded in place when hora starts):

| Table | Columns |
|---|---|
| `project` | `id`, `slug` (unique, stable), `name`, `client`, `billable_default`, `archived`, `version`, `created_at`, `updated_at` |
| `entry` | `id` (UUID v7, stable), `project_id`, `date` (local `YYYY-MM-DD`), `started_at` and `ended_at` (RFC 3339 with the recorded offset, or null), `start_ms` and `end_ms` (epoch ms), `minutes` (null while a timer runs), `description`, `billable`, `state` (`draft` or `confirmed`), `author`, `refs_json`, `import_key`, `deleted_at` (deleted entries are kept), `version`, `created_at`, `updated_at` |
| `revision` | `id`, `entry_id`, `version`, `actor`, `via` (`ui` or `agent`), `action` (`create`, `update`, `confirm`, `delete`, `start`, `stop`), `changes_json` (`{field: [before, after]}`), `reason`, `at`, `proposal_id` and `proposed_by` (set when the change was an accepted proposal; `actor` is then who accepted it) |
| `proposal` | `id`, `entry_id`, `entry_version` (the version it was made against), `action` (`update` or `delete`), `input_json` (the edit to apply), `changes_json` (`{field: [before, after]}`), `reason`, `author`, `state` (`pending`, `accepted`, `rejected`, `withdrawn`), `note`, `decided_by`, `created_at`, `decided_at` |

Billable time is `state = 'confirmed' AND billable = 1 AND deleted_at IS NULL AND minutes IS NOT NULL`. hora keeps no rates, invoices or billed flags; an invoicing tool keys its own records on entry IDs.

## Importing from kritee

```sh
hora import kritee ~/Downloads/kritee --timezone America/New_York \
  --zone 2026-06-24=Europe/Berlin --zone 2026-08-20=America/New_York --dry-run
```

kritee stored times without an offset. Its timer entries are UTC; typed entries are wall-clock time wherever you were. `--timezone` is the default for typed times and each `--zone DATE=Area/City` applies from that date on. `--dry-run` prints what would be sent. Add the projects in the UI first. Entries arrive as drafts by the agent you name, keyed by their kritee IDs, so the import can be re-run.

## Development

```sh
bun run --cwd spa build
cargo build --locked
cargo test --locked
cargo clippy --all-targets
bun run --cwd spa check
bun test --cwd spa
```

`bun run --cwd spa dev` serves the UI with hot reload and proxies `/api` to a hora on port 4530. Operations live in `src/ops.rs`, every rule about who may change what in `src/store.rs`, and the one place the caller is decided in `actor_for` in `src/http.rs`.

## License

[MIT](LICENSE)
