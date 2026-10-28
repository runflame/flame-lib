//! The bots' own record, in SQLite: what the playground's ledger does not
//! keep. Every decision with its reasons, every refusal and outage, the
//! settings, and a telemetry sample a minute. Nothing is ever trimmed: a
//! week of it is a few megabytes, and it lives on disk, not in memory.

use std::{path::Path, time::Duration};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub type Error = rusqlite::Error;

pub struct Journal {
    connection: Connection,
}

/// A contribution as the bot meant it, recorded before it is sent: a crash
/// or an outage retries the same request ID, which the playground answers
/// with the original contribution if it already took it.
#[derive(Clone, Debug, Serialize)]
pub struct Submission {
    pub request_id: String,
    pub bot: String,
    pub epoch: i64,
    /// Unix milliseconds.
    pub created_at: i64,
    pub budget_sats: i64,
    pub duration: i64,
    pub delay_blocks: i64,
    pub mode: String,
    pub reason: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub id: i64,
    /// Unix milliseconds.
    pub at: i64,
    pub bot: Option<String>,
    pub kind: String,
    pub height: Option<i64>,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Sample {
    /// Unix milliseconds.
    pub at: i64,
    pub bot: String,
    pub height: i64,
    /// Sats of every contribution the bot made in this ledger, pending ones included.
    pub committed_sats: i64,
    /// The minimax bot's budget and what its pace allows by now.
    pub budget_sats: Option<i64>,
    pub allowance_sats: Option<i64>,
    /// The playground's lifetime totals for the bot.
    pub burned_sats: i64,
    pub fees_sats: i64,
    /// Own rewards and commissions.
    pub reward_sparks: i64,
    pub own_burn: i64,
    pub delegated_burn: i64,
    pub delegated_budget_sats: i64,
    pub delegators: i64,
    pub network_burn: i64,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::from_connection(Connection::open(path)?)
    }

    fn from_connection(connection: Connection) -> Result<Self, Error> {
        // A second owner would run a second set of bots under the same names.
        connection.busy_timeout(Duration::ZERO)?;
        connection.execute_batch(
            "PRAGMA locking_mode = EXCLUSIVE;
             BEGIN EXCLUSIVE;
             CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY,
                 at INTEGER NOT NULL,
                 bot TEXT,
                 kind TEXT NOT NULL,
                 height INTEGER,
                 message TEXT NOT NULL,
                 data TEXT
             ) STRICT;
             CREATE INDEX IF NOT EXISTS events_by_bot ON events(bot, id);
             CREATE INDEX IF NOT EXISTS events_by_kind ON events(kind, id);
             CREATE TABLE IF NOT EXISTS submissions (
                 request_id TEXT PRIMARY KEY,
                 bot TEXT NOT NULL,
                 epoch INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 budget_sats INTEGER NOT NULL,
                 duration INTEGER NOT NULL,
                 delay_blocks INTEGER NOT NULL,
                 mode TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending'
                     CHECK(status IN ('pending', 'accepted', 'rejected')),
                 resolved_at INTEGER,
                 contribution_id INTEGER,
                 start_block INTEGER,
                 response TEXT
             ) STRICT;
             CREATE INDEX IF NOT EXISTS submissions_by_bot ON submissions(bot, epoch, created_at);
             CREATE TABLE IF NOT EXISTS samples (
                 at INTEGER NOT NULL,
                 bot TEXT NOT NULL,
                 height INTEGER NOT NULL,
                 committed_sats INTEGER NOT NULL,
                 budget_sats INTEGER,
                 allowance_sats INTEGER,
                 burned_sats INTEGER NOT NULL,
                 fees_sats INTEGER NOT NULL,
                 reward_sparks INTEGER NOT NULL,
                 own_burn INTEGER NOT NULL,
                 delegated_burn INTEGER NOT NULL,
                 delegated_budget_sats INTEGER NOT NULL,
                 delegators INTEGER NOT NULL,
                 network_burn INTEGER NOT NULL,
                 PRIMARY KEY (bot, at)
             ) STRICT;
             COMMIT;",
        )?;
        Ok(Self { connection })
    }

    pub fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>, Error> {
        let value: Option<String> = self
            .connection
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        // A setting this build cannot read is treated as absent.
        Ok(value.and_then(|value| serde_json::from_str(&value).ok()))
    }

    pub fn set_setting(&self, key: &str, value: &impl Serialize) -> Result<(), Error> {
        let value = serde_json::to_string(value).expect("settings always serialize");
        self.connection.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn log(
        &self,
        at: i64,
        bot: Option<&str>,
        kind: &str,
        height: Option<i64>,
        message: &str,
        data: Option<&serde_json::Value>,
    ) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO events(at, bot, kind, height, message, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![at, bot, kind, height, message, data.map(|data| data.to_string())],
        )?;
        Ok(())
    }

    /// Newest first, older than `before` if given. `filter` is a bot name
    /// or `errors`.
    pub fn events(
        &self,
        before: Option<i64>,
        filter: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Event>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, at, bot, kind, height, message, data FROM events
             WHERE id < ?1
               AND (?2 IS NULL OR (?2 = 'errors' AND kind IN ('error', 'rejected')) OR bot = ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let events = statement
            .query_map(params![before.unwrap_or(i64::MAX), filter, limit], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    bot: row.get(2)?,
                    kind: row.get(3)?,
                    height: row.get(4)?,
                    message: row.get(5)?,
                    data: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|data| serde_json::from_str(&data).ok()),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(events)
    }

    pub fn add_submission(&self, submission: &Submission) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO submissions(request_id, bot, epoch, created_at, budget_sats, duration,
                 delay_blocks, mode, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                submission.request_id,
                submission.bot,
                submission.epoch,
                submission.created_at,
                submission.budget_sats,
                submission.duration,
                submission.delay_blocks,
                submission.mode,
                submission.reason.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Accepted with the playground's contribution ID and start block, or
    /// rejected with its message.
    pub fn resolve_submission(
        &self,
        request_id: &str,
        at: i64,
        accepted: Result<(i64, i64), &str>,
    ) -> Result<(), Error> {
        let (status, contribution_id, start_block, response) = match accepted {
            Ok((id, start)) => ("accepted", Some(id), Some(start), None),
            Err(message) => ("rejected", None, None, Some(message)),
        };
        self.connection.execute(
            "UPDATE submissions SET status = ?2, resolved_at = ?3, contribution_id = ?4,
                 start_block = ?5, response = ?6
             WHERE request_id = ?1 AND status = 'pending'",
            params![
                request_id,
                status,
                at,
                contribution_id,
                start_block,
                response
            ],
        )?;
        Ok(())
    }

    pub fn pending(&self, bot: &str) -> Result<Option<Submission>, Error> {
        self.connection
            .query_row(
                "SELECT request_id, bot, epoch, created_at, budget_sats, duration, delay_blocks,
                     mode, reason
                 FROM submissions WHERE bot = ?1 AND status = 'pending'
                 ORDER BY created_at LIMIT 1",
                [bot],
                |row| {
                    Ok(Submission {
                        request_id: row.get(0)?,
                        bot: row.get(1)?,
                        epoch: row.get(2)?,
                        created_at: row.get(3)?,
                        budget_sats: row.get(4)?,
                        duration: row.get(5)?,
                        delay_blocks: row.get(6)?,
                        mode: row.get(7)?,
                        reason: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
                    })
                },
            )
            .optional()
    }

    /// Sats the bot committed in ledger `epoch` since `since` (unix
    /// milliseconds), counting pending submissions as spent.
    pub fn committed(&self, bot: &str, epoch: i64, since: i64) -> Result<i64, Error> {
        self.connection.query_row(
            "SELECT COALESCE(SUM(budget_sats), 0) FROM submissions
             WHERE bot = ?1 AND epoch = ?2 AND created_at >= ?3 AND status != 'rejected'",
            params![bot, epoch, since],
            |row| row.get(0),
        )
    }

    pub fn add_sample(&self, sample: &Sample) -> Result<(), Error> {
        self.connection.execute(
            "INSERT OR REPLACE INTO samples(at, bot, height, committed_sats, budget_sats,
                 allowance_sats, burned_sats, fees_sats, reward_sparks, own_burn, delegated_burn,
                 delegated_budget_sats, delegators, network_burn)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                sample.at,
                sample.bot,
                sample.height,
                sample.committed_sats,
                sample.budget_sats,
                sample.allowance_sats,
                sample.burned_sats,
                sample.fees_sats,
                sample.reward_sparks,
                sample.own_burn,
                sample.delegated_burn,
                sample.delegated_budget_sats,
                sample.delegators,
                sample.network_burn,
            ],
        )?;
        Ok(())
    }

    /// The bot's samples since `since`, thinned evenly to about `points`,
    /// always keeping the latest.
    pub fn samples(&self, bot: &str, since: i64, points: i64) -> Result<Vec<Sample>, Error> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM samples WHERE bot = ?1 AND at >= ?2",
            params![bot, since],
            |row| row.get(0),
        )?;
        let step = ((count + points - 1) / points.max(1)).max(1);
        let mut statement = self.connection.prepare(
            "SELECT at, bot, height, committed_sats, budget_sats, allowance_sats, burned_sats,
                 fees_sats, reward_sparks, own_burn, delegated_burn, delegated_budget_sats,
                 delegators, network_burn
             FROM (SELECT *, ROW_NUMBER() OVER (ORDER BY at) - 1 AS n
                   FROM samples WHERE bot = ?1 AND at >= ?2)
             WHERE n % ?3 = 0 OR n = ?4 - 1 ORDER BY at",
        )?;
        let samples = statement
            .query_map(params![bot, since, step, count], |row| {
                Ok(Sample {
                    at: row.get(0)?,
                    bot: row.get(1)?,
                    height: row.get(2)?,
                    committed_sats: row.get(3)?,
                    budget_sats: row.get(4)?,
                    allowance_sats: row.get(5)?,
                    burned_sats: row.get(6)?,
                    fees_sats: row.get(7)?,
                    reward_sparks: row.get(8)?,
                    own_burn: row.get(9)?,
                    delegated_burn: row.get(10)?,
                    delegated_budget_sats: row.get(11)?,
                    delegators: row.get(12)?,
                    network_burn: row.get(13)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Journal {
        Journal::from_connection(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn submission(request_id: &str, created_at: i64, budget_sats: i64) -> Submission {
        Submission {
            request_id: request_id.into(),
            bot: "minimax".into(),
            epoch: 0,
            created_at,
            budget_sats,
            duration: 144,
            delay_blocks: 0,
            mode: "service".into(),
            reason: serde_json::json!({"price": 1.5}),
        }
    }

    #[test]
    fn submissions_stay_pending_until_resolved_once_and_count_unless_rejected() {
        let journal = journal();
        journal
            .add_submission(&submission("first", 10, 500))
            .unwrap();
        let pending = journal.pending("minimax").unwrap().unwrap();
        assert_eq!(pending.request_id, "first");
        assert_eq!(pending.reason, serde_json::json!({"price": 1.5}));
        assert_eq!(journal.committed("minimax", 0, 0).unwrap(), 500);

        journal
            .resolve_submission("first", 20, Ok((7, 100)))
            .unwrap();
        // A late duplicate answer does not overwrite the first.
        journal.resolve_submission("first", 30, Err("no")).unwrap();
        assert!(journal.pending("minimax").unwrap().is_none());
        assert_eq!(journal.committed("minimax", 0, 0).unwrap(), 500);

        journal
            .add_submission(&submission("second", 40, 300))
            .unwrap();
        journal
            .resolve_submission("second", 50, Err("refused"))
            .unwrap();
        journal
            .add_submission(&submission("third", 60, 200))
            .unwrap();
        assert_eq!(journal.committed("minimax", 0, 0).unwrap(), 700);
        assert_eq!(journal.committed("minimax", 0, 41).unwrap(), 200);
        assert_eq!(journal.committed("minimax", 1, 0).unwrap(), 0);
        assert_eq!(journal.committed("clock", 0, 0).unwrap(), 0);
        // The same request ID is never recorded twice.
        assert!(journal
            .add_submission(&submission("third", 70, 200))
            .is_err());
    }

    #[test]
    fn events_page_newest_first_by_bot_or_errors() {
        let journal = journal();
        for (index, (bot, kind)) in [
            (None, "start"),
            (Some("clock"), "submitted"),
            (Some("minimax"), "error"),
            (Some("minimax"), "rejected"),
            (Some("clock"), "recovered"),
        ]
        .into_iter()
        .enumerate()
        {
            let data = serde_json::json!({"n": index});
            journal
                .log(index as i64, bot, kind, Some(5), kind, Some(&data))
                .unwrap();
        }
        let ids = |events: Vec<Event>| events.iter().map(|event| event.id).collect::<Vec<_>>();
        assert_eq!(
            ids(journal.events(None, None, 100).unwrap()),
            [5, 4, 3, 2, 1]
        );
        assert_eq!(ids(journal.events(Some(4), None, 2).unwrap()), [3, 2]);
        assert_eq!(
            ids(journal.events(None, Some("clock"), 100).unwrap()),
            [5, 2]
        );
        assert_eq!(
            ids(journal.events(None, Some("errors"), 100).unwrap()),
            [4, 3]
        );
        let first = journal.events(Some(2), None, 1).unwrap().remove(0);
        assert_eq!(first.bot, None);
        assert_eq!(first.data, Some(serde_json::json!({"n": 0})));
    }

    #[test]
    fn samples_thin_evenly_and_keep_the_latest() {
        let journal = journal();
        for at in 0..10 {
            journal
                .add_sample(&Sample {
                    at,
                    bot: "clock".into(),
                    height: at * 60,
                    ..Default::default()
                })
                .unwrap();
        }
        let at = |samples: Vec<Sample>| samples.iter().map(|sample| sample.at).collect::<Vec<_>>();
        assert_eq!(
            at(journal.samples("clock", 0, 100).unwrap()),
            (0..10).collect::<Vec<_>>()
        );
        assert_eq!(at(journal.samples("clock", 0, 4).unwrap()), [0, 3, 6, 9]);
        assert_eq!(at(journal.samples("clock", 0, 3).unwrap()), [0, 4, 8, 9]);
        assert_eq!(
            at(journal.samples("clock", 5, 100).unwrap()),
            [5, 6, 7, 8, 9]
        );
        assert!(journal.samples("minimax", 0, 100).unwrap().is_empty());
    }

    #[test]
    fn settings_round_trip_and_a_second_owner_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bots.sqlite3");
        let journal = Journal::open(&path).unwrap();
        assert_eq!(journal.setting::<i64>("epoch").unwrap(), None);
        journal.set_setting("epoch", &3).unwrap();
        journal.set_setting("epoch", &4).unwrap();
        assert_eq!(journal.setting::<i64>("epoch").unwrap(), Some(4));
        assert!(Journal::open(&path).is_err());
        drop(journal);
        assert_eq!(
            Journal::open(&path)
                .unwrap()
                .setting::<i64>("epoch")
                .unwrap(),
            Some(4)
        );
    }
}
