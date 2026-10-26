use std::{fmt, path::Path, time::Duration};

use flamekd::{Network, ReceivingAddress};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

pub const FEE_PER_BLOCK: i64 = 1_000;
pub const BLOCK_REWARD_SPARKS: i64 = 5_000_000_000;
/// Block 1 begins at 2027-01-11T00:00:00Z, the calendar the UI shows.
pub const CALENDAR_START: i64 = 1_799_625_600;
/// Seconds per simulated block: 144 blocks a day.
pub const BLOCK_SECONDS: i64 = 600;
/// Most blocks one `/api/core/blocks` request returns.
pub const MAX_CORE_BLOCKS: i64 = 1_024;
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Independent,
    Service,
    Delegating,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::Service => "service",
            Self::Delegating => "delegating",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Conflict(String),
    Storage(rusqlite::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Conflict(message) => f.write_str(message),
            Self::Storage(error) => write!(f, "Database error: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionRequest {
    pub address: String,
    pub budget_sats: i64,
    pub duration: i64,
    #[serde(default)]
    pub delay_blocks: i64,
    pub request_id: String,
    #[serde(default)]
    pub mode: Mode,
    // Old clients may replay this field, but new contributions cannot specify a rate.
    #[serde(default, deserialize_with = "legacy_commission")]
    pub commission_bps: Option<i64>,
    pub service_user_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Submitted {
    pub id: i64,
    pub user_id: i64,
    pub start_block: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinterRequest {
    pub address: String,
    pub name: Option<String>,
    pub service_commission_bps: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Minter {
    pub user_id: i64,
    pub name: String,
    pub service_commission_bps: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub contribution_count: i64,
    pub burned_sats: String,
    pub fees_sats: String,
    pub reward_sparks: String,
}

#[derive(Debug, Serialize)]
pub struct Contribution {
    pub id: i64,
    pub user_id: i64,
    pub address: String,
    pub budget_sats: i64,
    pub duration: i64,
    pub delay_blocks: i64,
    pub start_block: i64,
    pub burned_sats: String,
    pub fees_sats: String,
    pub reward_sparks: String,
    pub user_name: String,
    pub minter_id: i64,
    pub minter_name: String,
    pub mode: String,
    pub commission_bps: i64,
    pub service_user_id: Option<i64>,
    pub commission_sparks: String,
}

#[derive(Debug, Serialize)]
pub struct Schedule {
    pub id: i64,
    pub user_id: i64,
    pub budget_sats: i64,
    pub duration: i64,
    pub start_block: i64,
    pub mode: String,
    pub commission_bps: i64,
    pub service_user_id: Option<i64>,
    pub minter_id: i64,
}

#[derive(Debug, Serialize)]
pub struct Service {
    pub user_id: i64,
    pub name: String,
    pub commission_bps: i64,
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct MinterSummary {
    pub user_id: i64,
    pub address: String,
    pub name: String,
    pub mode: String,
    pub commission_bps: i64,
    pub burned_sats: String,
    pub fees_sats: String,
    pub reward_sparks: String,
    pub contribution_count: i64,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub height: i64,
    pub totals: Totals,
    pub contributions: Vec<Contribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart_contributions: Option<Vec<Schedule>>,
    pub fee_per_block: i64,
    pub block_reward_sparks: i64,
    pub services: Vec<Service>,
    pub minters: Vec<MinterSummary>,
}

/// The last committed block, as a Flame node's core indexer sees it.
#[derive(Debug, Serialize)]
pub struct CoreTip {
    pub height: i64,
    pub time: i64,
}

/// One committed block and the burns eligible for its reward, in contribution order.
#[derive(Debug, Serialize)]
pub struct CoreBlock {
    pub height: i64,
    pub time: i64,
    pub burns: Vec<CoreBurn>,
}

/// One contribution's eligible burn in a block. The chain pays its share to `address`, less
/// the commission a delegated burn's `service` takes.
#[derive(Debug, Serialize)]
pub struct CoreBurn {
    pub address: String,
    pub sats: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<CoreService>,
}

/// A delegated burn's terms: its provider's address and the commission in force in that block.
#[derive(Debug, Serialize)]
pub struct CoreService {
    pub address: String,
    pub commission_bps: i64,
}

#[derive(Debug, Serialize)]
pub struct RewardBalance {
    pub address: String,
    pub reward_sparks: String,
}

pub struct Database {
    connection: Connection,
}

pub fn reset(
    path: &Path,
    output: &mut impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = path.canonicalize()?;
    if !path.is_file() {
        return Err(std::io::Error::other("Reset requires an existing database file.").into());
    }
    let connection =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::ZERO)?;
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'
             AND name IN ('accounts', 'contributions', 'blocks', 'accruals')",
        [],
        |row| row.get(0),
    )?;
    if tables != 4 {
        return Err(std::io::Error::other("Reset requires a mint-playground database.").into());
    }
    connection.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
    let (backup_path, backup_file) = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let mut name = path
            .file_name()
            .ok_or_else(|| std::io::Error::other("Database has no filename."))?
            .to_os_string();
        name.push(format!(".backup-{nanos}"));
        let backup_path = path.with_file_name(name);
        if backup_path
            .to_str()
            .is_none_or(|path| path.contains(['\r', '\n']))
        {
            return Err(
                std::io::Error::other("Backup path must be valid single-line UTF-8.").into(),
            );
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&backup_path) {
            Ok(file) => break (backup_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    connection.execute("VACUUM INTO ?1", [backup_path.to_str().unwrap()])?;
    backup_file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(backup_path.parent().unwrap())?.sync_all()?;
    writeln!(output, "{}", backup_path.display())?;
    output.flush()?;

    // Keep the same exclusive owner from backup through migrations and the atomic reset.
    let mut database = Database::from_connection(connection)?;
    let transaction = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Exclusive)?;
    transaction.execute_batch("DELETE FROM accruals; DELETE FROM contributions; DELETE FROM blocks; DELETE FROM accounts;")?;
    transaction.commit()?;
    Ok(())
}

impl Database {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::from_connection(Connection::open(path)?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, Error> {
        // A second owner would run a second minting clock against the same ledger.
        connection.busy_timeout(Duration::ZERO)?;
        // Stored durations also accept historical 100-block records; submit enforces new limits.
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA locking_mode = EXCLUSIVE;
             BEGIN EXCLUSIVE;
             CREATE TABLE IF NOT EXISTS accounts (
                 id INTEGER PRIMARY KEY,
                 address TEXT NOT NULL UNIQUE,
                 name TEXT,
                 reward_sparks INTEGER NOT NULL DEFAULT 0
                     CHECK(typeof(reward_sparks) = 'integer' AND reward_sparks >= 0)
             );
             CREATE TABLE IF NOT EXISTS contributions (
                 id INTEGER PRIMARY KEY,
                 request_id TEXT NOT NULL UNIQUE,
                 user_id INTEGER NOT NULL REFERENCES accounts(id),
                 budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                     AND budget_sats BETWEEN 2000 AND 9007199254740991),
                 duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                     AND duration BETWEEN 100 AND 9007199254740991),
                 delay_blocks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(delay_blocks) = 'integer'
                     AND delay_blocks BETWEEN 0 AND 9007199254740991),
                 start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                     AND start_block >= 1 AND start_block <= 9007199254740991 - duration),
                 CHECK(budget_sats / duration >= 1001)
             );
             CREATE TABLE IF NOT EXISTS blocks (
                 height INTEGER PRIMARY KEY CHECK(height >= 1),
                 burned_sats INTEGER NOT NULL CHECK(typeof(burned_sats) = 'integer'
                     AND burned_sats >= 0),
                 fees_sats INTEGER NOT NULL CHECK(typeof(fees_sats) = 'integer'
                     AND fees_sats >= 0),
                 reward_sparks INTEGER NOT NULL CHECK(typeof(reward_sparks) = 'integer'
                     AND ((burned_sats = 0 AND reward_sparks = 0)
                         OR (burned_sats > 0 AND reward_sparks = 5000000000)))
             );
             CREATE TABLE IF NOT EXISTS accruals (
                 height INTEGER NOT NULL REFERENCES blocks(height),
                 contribution_id INTEGER NOT NULL REFERENCES contributions(id),
                 burned_sats INTEGER NOT NULL CHECK(typeof(burned_sats) = 'integer'
                     AND burned_sats > 0),
                 fees_sats INTEGER NOT NULL CHECK(fees_sats = 1000),
                 reward_sparks INTEGER NOT NULL CHECK(typeof(reward_sparks) = 'integer'
                     AND reward_sparks >= 0),
                 PRIMARY KEY(height, contribution_id)
             );
             CREATE INDEX IF NOT EXISTS accruals_by_contribution
                 ON accruals(contribution_id);
             COMMIT;",
        )?;
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version < 1 {
            let has_delay: bool = connection.query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('contributions') WHERE name = 'delay_blocks'",
                [],
                |row| row.get(0),
            )?;
            // Rebuild only the constrained table, retaining all ledger IDs and references.
            connection.pragma_update(None, "foreign_keys", false)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
            transaction.execute_batch(
                "CREATE TABLE contributions_new (
                     id INTEGER PRIMARY KEY,
                     request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id),
                     budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                         AND budget_sats BETWEEN 2000 AND 9007199254740991),
                     duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                         AND duration BETWEEN 100 AND 9007199254740991),
                     delay_blocks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(delay_blocks) = 'integer'
                         AND delay_blocks BETWEEN 0 AND 9007199254740991),
                     start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                         AND start_block >= 1 AND start_block <= 9007199254740991 - duration),
                     CHECK(budget_sats / duration >= 1001)
                 );",
            )?;
            transaction.execute_batch(&format!(
                "INSERT INTO contributions_new(id, request_id, user_id, budget_sats, duration, start_block, delay_blocks)
                     SELECT id, request_id, user_id, budget_sats, duration, start_block, {} FROM contributions;
                 DROP TABLE contributions;
                 ALTER TABLE contributions_new RENAME TO contributions;",
                if has_delay { "delay_blocks" } else { "0" }
            ))?;
            if transaction
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?
            {
                return Err(Error::Invalid(
                    "Database migration found invalid ledger references.".into(),
                ));
            }
            transaction.pragma_update(None, "user_version", 1)?;
            transaction.commit()?;
            connection.pragma_update(None, "foreign_keys", true)?;
        }
        let has_name: bool = connection.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('accounts') WHERE name = 'name'",
            [],
            |row| row.get(0),
        )?;
        if !has_name {
            connection.execute_batch("ALTER TABLE accounts ADD COLUMN name TEXT;")?;
        }
        if schema_version < 2 {
            connection.pragma_update(None, "foreign_keys", false)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
            let has_service: bool = transaction.query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('accounts') WHERE name = 'service_commission_bps'", [], |row| row.get(0)
            )?;
            if !has_service {
                transaction.execute_batch("ALTER TABLE accounts ADD COLUMN service_commission_bps INTEGER
                     CHECK(service_commission_bps IS NULL OR (typeof(service_commission_bps) = 'integer'
                         AND service_commission_bps BETWEEN 0 AND 10000));")?;
            }
            transaction.execute_batch(
                "CREATE TABLE contributions_v2 (
                     id INTEGER PRIMARY KEY,
                     request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id),
                     budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                         AND budget_sats BETWEEN 2000 AND 9007199254740991),
                     duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                         AND duration BETWEEN 100 AND 9007199254740991),
                     delay_blocks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(delay_blocks) = 'integer'
                         AND delay_blocks BETWEEN 0 AND 9007199254740991),
                     start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                         AND start_block >= 1 AND start_block <= 9007199254740991 - duration),
                     mode TEXT NOT NULL DEFAULT 'independent' CHECK(mode IN ('independent', 'service', 'delegating')),
                     commission_bps INTEGER NOT NULL DEFAULT 0 CHECK(typeof(commission_bps) = 'integer'
                         AND commission_bps BETWEEN 0 AND 10000),
                     service_user_id INTEGER REFERENCES accounts(id),
                     CHECK((mode = 'independent' AND commission_bps = 0 AND service_user_id IS NULL)
                         OR (mode = 'service' AND service_user_id IS NULL)
                         OR (mode = 'delegating' AND service_user_id IS NOT NULL)),
                     CHECK(budget_sats / duration >= CASE WHEN mode = 'delegating' THEN 1 ELSE 1001 END)
                 );
                 INSERT INTO contributions_v2(id, request_id, user_id, budget_sats, duration, start_block, delay_blocks)
                     SELECT id, request_id, user_id, budget_sats, duration, start_block, delay_blocks FROM contributions;
                 CREATE TABLE blocks_v2 (
                     height INTEGER PRIMARY KEY CHECK(height >= 1),
                     burned_sats INTEGER NOT NULL CHECK(typeof(burned_sats) = 'integer' AND burned_sats >= 0),
                     fees_sats INTEGER NOT NULL CHECK(typeof(fees_sats) = 'integer' AND fees_sats >= 0),
                     reward_sparks INTEGER NOT NULL CHECK(typeof(reward_sparks) = 'integer'
                         AND reward_sparks IN (0, 5000000000) AND (reward_sparks = 0 OR burned_sats > 0))
                 );
                 INSERT INTO blocks_v2 SELECT * FROM blocks;
                 CREATE TABLE accruals_v2 (
                     height INTEGER NOT NULL REFERENCES blocks(height),
                     contribution_id INTEGER NOT NULL REFERENCES contributions(id),
                     burned_sats INTEGER NOT NULL CHECK(typeof(burned_sats) = 'integer' AND burned_sats > 0),
                     fees_sats INTEGER NOT NULL CHECK(fees_sats IN (0, 1000)),
                     reward_sparks INTEGER NOT NULL CHECK(typeof(reward_sparks) = 'integer' AND reward_sparks >= 0),
                     commission_sparks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(commission_sparks) = 'integer'
                         AND commission_sparks BETWEEN 0 AND reward_sparks),
                     PRIMARY KEY(height, contribution_id)
                 );
                 INSERT INTO accruals_v2(height, contribution_id, burned_sats, fees_sats, reward_sparks)
                     SELECT height, contribution_id, burned_sats, fees_sats, reward_sparks FROM accruals;
                 DROP TABLE accruals;
                 DROP TABLE contributions;
                 DROP TABLE blocks;
                 ALTER TABLE contributions_v2 RENAME TO contributions;
                 ALTER TABLE blocks_v2 RENAME TO blocks;
                 ALTER TABLE accruals_v2 RENAME TO accruals;
                 CREATE INDEX accruals_by_contribution ON accruals(contribution_id);",
            )?;
            if transaction
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?
            {
                return Err(Error::Invalid(
                    "Database migration found invalid ledger references.".into(),
                ));
            }
            transaction.pragma_update(None, "user_version", 2)?;
            transaction.commit()?;
            connection.pragma_update(None, "foreign_keys", true)?;
        }
        if schema_version < 3 {
            connection.pragma_update(None, "foreign_keys", false)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
            transaction.execute_batch(
                "CREATE TABLE contributions_v3 (
                     id INTEGER PRIMARY KEY,
                     request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id),
                     budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                         AND budget_sats BETWEEN 2000 AND 9007199254740991),
                     duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                         AND duration BETWEEN 100 AND 9007199254740991),
                     delay_blocks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(delay_blocks) = 'integer'
                         AND delay_blocks BETWEEN 0 AND 9007199254740991),
                     start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                         AND start_block >= 1 AND start_block <= 9007199254740991 - duration),
                     mode TEXT NOT NULL DEFAULT 'independent' CHECK(mode IN ('independent', 'service', 'delegating')),
                     service_user_id INTEGER REFERENCES accounts(id),
                     CHECK((mode IN ('independent', 'service') AND service_user_id IS NULL)
                         OR (mode = 'delegating' AND service_user_id IS NOT NULL)),
                     CHECK(budget_sats / duration >= CASE WHEN mode = 'delegating' THEN 1 ELSE 1001 END)
                 );
                 INSERT INTO contributions_v3(id, request_id, user_id, budget_sats, duration, start_block, delay_blocks, mode, service_user_id)
                     SELECT id, request_id, user_id, budget_sats, duration, start_block, delay_blocks, mode, service_user_id FROM contributions;
                 DROP TABLE contributions;
                 ALTER TABLE contributions_v3 RENAME TO contributions;",
            )?;
            reprice_commissions(&transaction, None)?;
            if transaction
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?
            {
                return Err(Error::Invalid(
                    "Database migration found invalid ledger references.".into(),
                ));
            }
            transaction.pragma_update(None, "user_version", 3)?;
            transaction.commit()?;
            connection.pragma_update(None, "foreign_keys", true)?;
        }
        if schema_version < 4 {
            // Each accrual keeps the commission in force in its block. Version 3 had
            // repriced every accrual to the provider's current rate, so that rate is
            // the one its stored commission was computed with.
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
            let has_rate: bool = transaction.query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('accruals') WHERE name = 'commission_bps'",
                [],
                |row| row.get(0),
            )?;
            if !has_rate {
                transaction.execute_batch(
                    "ALTER TABLE accruals ADD COLUMN commission_bps INTEGER NOT NULL DEFAULT 0
                         CHECK(typeof(commission_bps) = 'integer' AND commission_bps BETWEEN 0 AND 10000);",
                )?;
            }
            transaction.execute_batch(
                "UPDATE accruals SET commission_bps = (
                     SELECT COALESCE(u.service_commission_bps, 0)
                     FROM contributions c JOIN accounts u ON u.id = c.service_user_id
                     WHERE c.id = accruals.contribution_id)
                 WHERE contribution_id IN (SELECT id FROM contributions WHERE service_user_id IS NOT NULL);",
            )?;
            transaction.pragma_update(None, "user_version", 4)?;
            transaction.commit()?;
        }
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS contributions_by_user ON contributions(user_id);",
        )?;
        Ok(Self { connection })
    }

    pub fn minter(&mut self, request: MinterRequest) -> Result<Minter, Error> {
        let address = canonical_address(&request.address)?;
        if request
            .service_commission_bps
            .is_some_and(|rate| !(0..=10_000).contains(&rate))
        {
            return Err(Error::Invalid(
                "Service commission must be between 0 and 10,000 basis points.".into(),
            ));
        }
        let name = request
            .name
            .map(|name| {
                let trimmed = name.trim();
                if !(1..=64).contains(&trimmed.chars().count())
                    || name.chars().any(char::is_control)
                {
                    return Err(Error::Invalid(
                        "Name must contain 1–64 characters with no control characters.".into(),
                    ));
                }
                Ok(trimmed.to_owned())
            })
            .transpose()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO accounts(address) VALUES (?1)",
            [&address],
        )?;
        if let Some(name) = name {
            transaction.execute(
                "UPDATE accounts SET name = ?1 WHERE address = ?2",
                params![name, address],
            )?;
        }
        if let Some(rate) = request.service_commission_bps {
            let (user_id, previous): (i64, Option<i64>) = transaction.query_row(
                "SELECT id, service_commission_bps FROM accounts WHERE address = ?1",
                [&address],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            // The rate applies from the next block on; past accruals keep theirs.
            if previous != Some(rate) {
                transaction.execute(
                    "UPDATE accounts SET service_commission_bps = ?1 WHERE id = ?2",
                    params![rate, user_id],
                )?;
            }
        }
        let minter = transaction.query_row(
            "SELECT id, COALESCE(name, 'Minter ' || id), service_commission_bps FROM accounts WHERE address = ?1",
            [&address],
            |row| {
                Ok(Minter {
                    user_id: row.get(0)?,
                    name: row.get(1)?,
                    service_commission_bps: row.get(2)?,
                })
            },
        )?;
        transaction.commit()?;
        Ok(minter)
    }

    pub fn submit(&mut self, mut request: ContributionRequest) -> Result<Submitted, Error> {
        request.address = canonical_address(&request.address)?;
        if (request.mode != Mode::Delegating && request.service_user_id.is_some())
            || (request.mode == Mode::Delegating
                && !request
                    .service_user_id
                    .is_some_and(|id| (1..=MAX_SAFE_INTEGER).contains(&id)))
        {
            return Err(Error::Invalid(
                "Invalid mode, commission or service selection.".into(),
            ));
        }
        if !(16..=64).contains(&request.request_id.len())
            || !request
                .request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(Error::Invalid(
                "Submission ID must contain 16–64 ASCII letters, digits or hyphens.".into(),
            ));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT c.id, c.start_block, u.address, c.budget_sats, c.duration, c.user_id, c.delay_blocks,
                        c.mode, c.service_user_id
             FROM contributions c JOIN accounts u ON u.id = c.user_id WHERE c.request_id = ?1",
                [&request.request_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            id,
            start_block,
            address,
            budget,
            duration,
            user_id,
            delay_blocks,
            mode,
            service_user_id,
        )) = previous
        {
            return if address == request.address
                && budget == request.budget_sats
                && duration == request.duration
                && delay_blocks == request.delay_blocks
                && mode == request.mode.as_str()
                && service_user_id == request.service_user_id
            {
                Ok(Submitted {
                    id,
                    user_id,
                    start_block,
                })
            } else {
                Err(Error::Conflict(
                    "Submission ID has already been used with different parameters.".into(),
                ))
            };
        }
        if request.commission_bps.is_some() {
            return Err(Error::Invalid(
                "Commission belongs to the service identity; update it via /api/minter.".into(),
            ));
        }
        if !(2_000..=MAX_SAFE_INTEGER).contains(&request.budget_sats)
            || !(144..=MAX_SAFE_INTEGER).contains(&request.duration)
            || !(0..=MAX_SAFE_INTEGER).contains(&request.delay_blocks)
        {
            return Err(Error::Invalid(
                "Budget, duration or delay is outside the supported range.".into(),
            ));
        }
        let fee = if request.mode == Mode::Delegating {
            0
        } else {
            FEE_PER_BLOCK
        };
        if request.budget_sats / request.duration < fee + 1 {
            return Err(Error::Invalid(
                "Budget must cover mining fees and at least 1 sat burned per block.".into(),
            ));
        }
        let height: i64 =
            transaction.query_row("SELECT COALESCE(MAX(height), 0) FROM blocks", [], |row| {
                row.get(0)
            })?;
        let start_block = height
            .checked_add(1)
            .and_then(|start| start.checked_add(request.delay_blocks))
            .filter(|start| *start <= MAX_SAFE_INTEGER - request.duration)
            .ok_or_else(|| {
                Error::Invalid("Contribution schedule exceeds the supported block range.".into())
            })?;
        if request.mode == Mode::Delegating {
            let offered: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1 AND service_commission_bps IS NOT NULL)",
                [request.service_user_id], |row| row.get(0),
            )?;
            if !offered {
                return Err(Error::Invalid("Select an existing minting service.".into()));
            }
        }
        transaction.execute(
            "INSERT OR IGNORE INTO accounts(address) VALUES (?1)",
            [&request.address],
        )?;
        let user_id = transaction.query_row(
            "SELECT id FROM accounts WHERE address = ?1",
            [&request.address],
            |row| row.get(0),
        )?;
        if request.mode == Mode::Service {
            transaction.execute(
                "UPDATE accounts SET service_commission_bps = COALESCE(service_commission_bps, 200) WHERE id = ?1",
                [user_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO contributions(request_id, user_id, budget_sats, duration, start_block, delay_blocks,
                 mode, service_user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                request.request_id,
                user_id,
                request.budget_sats,
                request.duration,
                start_block,
                request.delay_blocks,
                request.mode.as_str(),
                request.service_user_id,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Submitted {
            id,
            user_id,
            start_block,
        })
    }

    pub fn tick(&mut self) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let height: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(height), 0) + 1 FROM blocks",
            [],
            |row| row.get(0),
        )?;
        let mut active = {
            let mut statement = transaction.prepare(
                "SELECT c.id, c.user_id, c.budget_sats, c.duration, c.start_block, c.mode,
                        COALESCE(m.service_commission_bps, 0), c.service_user_id
                 FROM contributions c LEFT JOIN accounts m ON m.id = c.service_user_id
                 WHERE c.start_block <= ?1 AND c.start_block + c.duration > ?1 ORDER BY c.id",
            )?;
            let active = statement
                .query_map([height], |row| {
                    let budget: i64 = row.get(2)?;
                    let duration: i64 = row.get(3)?;
                    let offset = height - row.get::<_, i64>(4)?;
                    let mode: String = row.get(5)?;
                    let fee = if mode == "delegating" {
                        0
                    } else {
                        FEE_PER_BLOCK
                    };
                    let net = budget - duration * fee;
                    Ok(Accrual {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        burn: net / duration + i64::from(offset < net % duration),
                        fee,
                        service: mode == "service",
                        commission_bps: row.get(6)?,
                        service_user_id: row.get(7)?,
                        eligible: true,
                        reward: 0,
                        remainder: 0,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            active
        };
        let services: std::collections::HashSet<_> = active
            .iter()
            .filter(|item| item.service)
            .map(|item| item.user_id)
            .collect();
        for item in &mut active {
            item.eligible = item.service_user_id.is_none_or(|id| services.contains(&id));
        }
        let total_burn: i128 = active.iter().map(|item| i128::from(item.burn)).sum();
        let eligible_burn: i128 = active
            .iter()
            .filter(|item| item.eligible)
            .map(|item| i128::from(item.burn))
            .sum();
        let total_fees: i128 = active.iter().map(|item| i128::from(item.fee)).sum();
        // With no eligible minting, issuance is permanently lost rather than carried forward.
        let reward = if eligible_burn == 0 {
            0
        } else {
            BLOCK_REWARD_SPARKS
        };
        if reward > 0 {
            for item in active.iter_mut().filter(|item| item.eligible) {
                let numerator = i128::from(item.burn) * i128::from(reward);
                item.reward = (numerator / eligible_burn) as i64;
                item.remainder = numerator % eligible_burn;
            }
            let remaining = reward - active.iter().map(|item| item.reward).sum::<i64>();
            active.sort_unstable_by(|a, b| b.remainder.cmp(&a.remainder).then(a.id.cmp(&b.id)));
            for item in active
                .iter_mut()
                .filter(|item| item.eligible)
                .take(remaining as usize)
            {
                item.reward += 1;
            }
        }
        let amount = |value| {
            i64::try_from(value)
                .map_err(|_| Error::Invalid("Integer accounting capacity exceeded.".into()))
        };
        transaction.execute(
            "INSERT INTO blocks(height, burned_sats, fees_sats, reward_sparks) VALUES (?1, ?2, ?3, ?4)",
            params![height, amount(total_burn)?, amount(total_fees)?, reward],
        )?;
        for item in active {
            // The provider's rate in this block is the delegation's term for it, fixed
            // once committed, as the chain fixes it once minted.
            let rate = if item.service_user_id.is_some() {
                item.commission_bps
            } else {
                0
            };
            let commission = (i128::from(item.reward) * i128::from(rate) / 10_000) as i64;
            transaction.execute(
                "INSERT INTO accruals(height, contribution_id, burned_sats, fees_sats, reward_sparks,
                     commission_sparks, commission_bps)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![height, item.id, item.burn, item.fee, item.reward, commission, rate],
            )?;
            // SQLite may promote an overflowing addition to REAL; the account CHECK rejects it.
            transaction.execute(
                "UPDATE accounts SET reward_sparks = reward_sparks + ?1 WHERE id = ?2",
                params![item.reward - commission, item.user_id],
            )?;
            if commission > 0 {
                transaction.execute(
                    "UPDATE accounts SET reward_sparks = reward_sparks + ?1 WHERE id = ?2",
                    params![commission, item.service_user_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn snapshot(&self, full_history: bool) -> Result<Snapshot, Error> {
        let (height, totals) = self.connection.query_row(
            "SELECT COALESCE(MAX(height), 0), COALESCE(SUM(burned_sats), 0),
                 COALESCE(SUM(fees_sats), 0), COALESCE(SUM(reward_sparks), 0),
                 (SELECT COUNT(*) FROM contributions) FROM blocks",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    Totals {
                        contribution_count: row.get(4)?,
                        burned_sats: row.get::<_, i64>(1)?.to_string(),
                        fees_sats: row.get::<_, i64>(2)?.to_string(),
                        reward_sparks: row.get::<_, i64>(3)?.to_string(),
                    },
                ))
            },
        )?;
        let mut statement = self.connection.prepare(
            "SELECT c.id, u.address, c.budget_sats, c.duration, c.start_block,
                 COALESCE(SUM(a.burned_sats), 0), COALESCE(SUM(a.fees_sats), 0),
                 COALESCE(SUM(a.reward_sparks - a.commission_sparks), 0), c.user_id, c.delay_blocks,
                 COALESCE(u.name, 'Minter ' || u.id), m.id, COALESCE(m.name, 'Minter ' || m.id),
                 c.mode, CASE WHEN c.mode = 'independent' THEN 0 ELSE COALESCE(m.service_commission_bps, 0) END,
                 c.service_user_id, COALESCE(SUM(a.commission_sparks), 0)
             FROM (SELECT * FROM contributions ORDER BY id DESC LIMIT ?1) c
             JOIN accounts u ON u.id = c.user_id
             JOIN accounts m ON m.id = COALESCE(c.service_user_id, c.user_id)
             LEFT JOIN accruals a ON a.contribution_id = c.id
             GROUP BY c.id ORDER BY c.id DESC",
        )?;
        let contributions = statement
            .query_map([if full_history { -1 } else { 200 }], |row| {
                Ok(Contribution {
                    id: row.get(0)?,
                    user_id: row.get(8)?,
                    address: row.get(1)?,
                    budget_sats: row.get(2)?,
                    duration: row.get(3)?,
                    delay_blocks: row.get(9)?,
                    start_block: row.get(4)?,
                    burned_sats: row.get::<_, i64>(5)?.to_string(),
                    fees_sats: row.get::<_, i64>(6)?.to_string(),
                    reward_sparks: row.get::<_, i64>(7)?.to_string(),
                    user_name: row.get(10)?,
                    minter_id: row.get(11)?,
                    minter_name: row.get(12)?,
                    mode: row.get(13)?,
                    commission_bps: row.get(14)?,
                    service_user_id: row.get(15)?,
                    commission_sparks: row.get::<_, i64>(16)?.to_string(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let chart_contributions = if full_history {
            None
        } else {
            let mut statement = self.connection.prepare(
                "SELECT c.id, c.user_id, c.budget_sats, c.duration, c.start_block,
                        c.mode, CASE WHEN c.mode = 'independent' THEN 0 ELSE COALESCE(m.service_commission_bps, 0) END,
                        c.service_user_id, m.id
                 FROM contributions c JOIN accounts m ON m.id = COALESCE(c.service_user_id, c.user_id)
                 ORDER BY c.id DESC",
            )?;
            let schedules = statement
                .query_map([], |row| {
                    Ok(Schedule {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        budget_sats: row.get(2)?,
                        duration: row.get(3)?,
                        start_block: row.get(4)?,
                        mode: row.get(5)?,
                        commission_bps: row.get(6)?,
                        service_user_id: row.get(7)?,
                        minter_id: row.get(8)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Some(schedules)
        };
        let mut statement = self.connection.prepare(
            "SELECT u.id, COALESCE(u.name, 'Minter ' || u.id), u.service_commission_bps,
                 EXISTS(SELECT 1 FROM contributions c WHERE c.user_id = u.id AND c.mode = 'service'
                     AND c.start_block <= ?1 AND c.start_block + c.duration > ?1)
             FROM accounts u WHERE u.service_commission_bps IS NOT NULL ORDER BY u.id",
        )?;
        let services = statement
            .query_map([height], |row| {
                Ok(Service {
                    user_id: row.get(0)?,
                    name: row.get(1)?,
                    commission_bps: row.get(2)?,
                    active: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare(
            "SELECT u.id, u.address, COALESCE(u.name, 'Minter ' || u.id), latest.mode,
                    CASE WHEN latest.mode = 'independent' THEN 0 ELSE COALESCE(m.service_commission_bps, 0) END,
                    COALESCE(SUM(a.burned_sats), 0), COALESCE(SUM(a.fees_sats), 0), u.reward_sparks,
                    COUNT(DISTINCT c.id)
             FROM accounts u JOIN contributions c ON c.user_id = u.id
             JOIN contributions latest ON latest.id = (SELECT MAX(id) FROM contributions WHERE user_id = u.id)
             JOIN accounts m ON m.id = COALESCE(latest.service_user_id, latest.user_id)
             LEFT JOIN accruals a ON a.contribution_id = c.id
             GROUP BY u.id ORDER BY u.id",
        )?;
        let minters = statement
            .query_map([], |row| {
                Ok(MinterSummary {
                    user_id: row.get(0)?,
                    address: row.get(1)?,
                    name: row.get(2)?,
                    mode: row.get(3)?,
                    commission_bps: row.get(4)?,
                    burned_sats: row.get::<_, i64>(5)?.to_string(),
                    fees_sats: row.get::<_, i64>(6)?.to_string(),
                    reward_sparks: row.get::<_, i64>(7)?.to_string(),
                    contribution_count: row.get(8)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(Snapshot {
            height,
            totals,
            contributions,
            chart_contributions,
            fee_per_block: FEE_PER_BLOCK,
            block_reward_sparks: BLOCK_REWARD_SPARKS,
            services,
            minters,
        })
    }

    pub fn rewards(&self) -> Result<Vec<RewardBalance>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT address, reward_sparks FROM accounts ORDER BY address")?;
        let rewards = statement
            .query_map([], |row| {
                Ok(RewardBalance {
                    address: row.get(0)?,
                    reward_sparks: row.get::<_, i64>(1)?.to_string(),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rewards)
    }
}

impl Database {
    pub fn core_tip(&self) -> Result<CoreTip, Error> {
        let height = self.connection.query_row(
            "SELECT COALESCE(MAX(height), 0) FROM blocks",
            [],
            |row| row.get(0),
        )?;
        Ok(CoreTip {
            height,
            time: block_time(height),
        })
    }

    /// Committed blocks `from..=to`, at most [`MAX_CORE_BLOCKS`], each with the burns `tick`
    /// counted as eligible: every Independent/Service burn, and a delegated burn only while its
    /// provider had an active Service contribution in that block, with that provider and the
    /// commission it took in that block. Fees and inactive delegations are spending, not weight.
    pub fn core_blocks(&self, from: i64, to: i64) -> Result<Vec<CoreBlock>, Error> {
        if from < 1 || to < from {
            return Err(Error::Invalid(
                "Expected 1 <= from <= to block heights.".into(),
            ));
        }
        let to = to.min(from.saturating_add(MAX_CORE_BLOCKS - 1));
        let mut blocks: Vec<CoreBlock> = self
            .connection
            .prepare("SELECT height FROM blocks WHERE height BETWEEN ?1 AND ?2 ORDER BY height")?
            .query_map(params![from, to], |row| row.get(0))?
            .map(|height| {
                height.map(|height| CoreBlock {
                    height,
                    time: block_time(height),
                    burns: Vec::new(),
                })
            })
            .collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare(
            "SELECT a.height, account.address, a.burned_sats, service.address, a.commission_bps
             FROM accruals a
             JOIN contributions c ON c.id = a.contribution_id
             JOIN accounts account ON account.id = c.user_id
             LEFT JOIN accounts service ON service.id = c.service_user_id
             WHERE a.height BETWEEN ?1 AND ?2 AND a.burned_sats > 0
               AND (c.service_user_id IS NULL OR EXISTS (
                   SELECT 1 FROM accruals s JOIN contributions sc ON sc.id = s.contribution_id
                   WHERE s.height = a.height AND sc.mode = 'service'
                     AND sc.user_id = c.service_user_id))
             ORDER BY a.height, a.contribution_id",
        )?;
        let burns = statement.query_map(params![from, to], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                CoreBurn {
                    address: row.get(1)?,
                    sats: row.get(2)?,
                    service: row
                        .get::<_, Option<String>>(3)?
                        .map(|address| -> rusqlite::Result<_> {
                            Ok(CoreService {
                                address,
                                commission_bps: row.get(4)?,
                            })
                        })
                        .transpose()?,
                },
            ))
        })?;
        for burn in burns {
            let (height, burn) = burn?;
            // Accruals exist only for committed blocks, which are contiguous from 1.
            let block = usize::try_from(height - from)
                .ok()
                .and_then(|index| blocks.get_mut(index))
                .filter(|block| block.height == height)
                .ok_or_else(|| Error::Invalid(format!("Block {height} is not contiguous.")))?;
            block.burns.push(burn);
        }
        Ok(blocks)
    }
}

fn block_time(height: i64) -> i64 {
    CALENDAR_START + (height.max(1) - 1) * BLOCK_SECONDS
}

fn canonical_address(address: &str) -> Result<String, Error> {
    ReceivingAddress::from_bech32(address, Network::Testnet)
        .map(|address| address.to_bech32(Network::Testnet))
        .map_err(|_| Error::Invalid("A valid testnet Flame receiving address is required.".into()))
}

fn legacy_commission<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i64>, D::Error> {
    // Preserve old null-valued retries while marking the obsolete field as present.
    Option::<i64>::deserialize(deserializer).map(|value| Some(value.unwrap_or(0)))
}

/// Version 3's migration: reprices every delegated accrual to its provider's current rate.
/// Since version 4 a rate applies only to later blocks, so nothing else calls this.
fn reprice_commissions(connection: &Connection, service_user_id: Option<i64>) -> Result<(), Error> {
    let mut deltas = std::collections::BTreeMap::<i64, i128>::new();
    let mut statement = connection.prepare(
        "SELECT a.reward_sparks, a.commission_sparks, c.user_id, c.service_user_id, u.service_commission_bps
         FROM accruals a JOIN contributions c ON c.id = a.contribution_id
         JOIN accounts u ON u.id = c.service_user_id
         WHERE ?1 IS NULL OR c.service_user_id = ?1",
    )?;
    let mut rows = statement.query([service_user_id])?;
    while let Some(row) = rows.next()? {
        let gross = i128::from(row.get::<_, i64>(0)?);
        let previous = i128::from(row.get::<_, i64>(1)?);
        let owner = row.get::<_, i64>(2)?;
        let service = row.get::<_, i64>(3)?;
        let rate = i128::from(row.get::<_, i64>(4)?);
        let delta = gross * rate / 10_000 - previous;
        *deltas.entry(owner).or_default() -= delta;
        *deltas.entry(service).or_default() += delta;
    }
    let mut balances = Vec::new();
    for (user_id, delta) in deltas.into_iter().filter(|(_, delta)| *delta != 0) {
        let current: i64 = connection.query_row(
            "SELECT reward_sparks FROM accounts WHERE id = ?1",
            [user_id],
            |row| row.get(0),
        )?;
        let balance = i128::from(current) + delta;
        let balance = i64::try_from(balance)
            .ok()
            .filter(|balance| *balance >= 0)
            .ok_or_else(|| {
                Error::Invalid(
                    "Reward accounting capacity exceeded; commission update was not applied."
                        .into(),
                )
            })?;
        balances.push((user_id, balance));
    }
    // Split multiplication so even cached i64 amounts never overflow SQL integer arithmetic.
    connection.execute(
        "UPDATE accruals SET commission_sparks = (
             SELECT accruals.reward_sparks / 10000 * u.service_commission_bps
                  + accruals.reward_sparks % 10000 * u.service_commission_bps / 10000
             FROM contributions c JOIN accounts u ON u.id = c.service_user_id
             WHERE c.id = accruals.contribution_id)
         WHERE contribution_id IN (SELECT id FROM contributions WHERE service_user_id IS NOT NULL
             AND (?1 IS NULL OR service_user_id = ?1))",
        [service_user_id],
    )?;
    for (user_id, balance) in balances {
        connection.execute(
            "UPDATE accounts SET reward_sparks = ?1 WHERE id = ?2",
            params![balance, user_id],
        )?;
    }
    Ok(())
}

struct Accrual {
    id: i64,
    user_id: i64,
    burn: i64,
    fee: i64,
    service: bool,
    commission_bps: i64,
    service_user_id: Option<i64>,
    eligible: bool,
    reward: i64,
    remainder: i128,
}

#[cfg(test)]
mod tests {
    use super::*;
    use flamekd::SpendKey;

    fn address(seed: u8) -> String {
        SpendKey::from_seed(&[seed; 64])
            .unwrap()
            .to_recv()
            .to_address()
            .to_bech32(Network::Testnet)
    }

    fn request(seed: u8, nonce: u8, net: i64) -> ContributionRequest {
        ContributionRequest {
            address: address(seed),
            budget_sats: 200_000 + net * 2,
            duration: 200,
            delay_blocks: 0,
            request_id: format!("submission-{nonce:016}"),
            mode: Mode::Independent,
            commission_bps: None,
            service_user_id: None,
        }
    }

    fn database() -> Database {
        Database::open(Path::new(":memory:")).unwrap()
    }

    struct ResetOutput<'a> {
        source: std::fs::File,
        original: &'a [u8],
        bytes: Vec<u8>,
        fail_write: bool,
        fail_flush: bool,
        flushed: bool,
    }

    impl std::io::Write for ResetOutput<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_write {
                return Err(std::io::Error::other("Output unavailable"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut original = Vec::new();
            std::io::Seek::rewind(&mut self.source)?;
            std::io::Read::read_to_end(&mut self.source, &mut original)?;
            assert_eq!(original, self.original);
            let path = std::path::PathBuf::from(std::str::from_utf8(&self.bytes).unwrap().trim());
            assert!(path.is_absolute());
            let backup =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            assert_eq!(
                backup
                    .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert_eq!(
                backup
                    .query_row("SELECT COUNT(*) FROM contributions", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                backup
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                2
            );
            self.flushed = true;
            if self.fail_flush {
                return Err(std::io::Error::other("Output flush failed"));
            }
            Ok(())
        }
    }

    #[test]
    fn reset_flushes_a_complete_backup_before_migration_or_deletion_and_aborts_on_output_errors() {
        for (fail_write, fail_flush) in [(false, false), (true, false), (false, true)] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("mint.sqlite3");
            {
                let mut db = Database::open(&path).unwrap();
                db.submit(request(1, 1, 100)).unwrap();
                db.tick().unwrap();
                db.connection.execute_batch("ALTER TABLE contributions ADD COLUMN commission_bps INTEGER DEFAULT 0; PRAGMA user_version=2;").unwrap();
            }
            let original = std::fs::read(&path).unwrap();
            let mut output = ResetOutput {
                source: std::fs::File::open(&path).unwrap(),
                original: &original,
                bytes: Vec::new(),
                fail_write,
                fail_flush,
                flushed: false,
            };
            let result = reset(&path, &mut output);
            if fail_write || fail_flush {
                assert!(result.is_err());
                assert_eq!(std::fs::read(&path).unwrap(), original);
            } else {
                result.unwrap();
                assert!(output.flushed);
                let mut db = Database::open(&path).unwrap();
                let state = db.snapshot(true).unwrap();
                assert_eq!(state.height, 0);
                assert_eq!(state.totals.contribution_count, 0);
                assert!(state.services.is_empty());
                assert!(state.minters.is_empty());
                assert!(db.rewards().unwrap().is_empty());
                let submitted = db.submit(request(2, 2, 100)).unwrap();
                assert_eq!(
                    (submitted.id, submitted.user_id, submitted.start_block),
                    (1, 1, 1)
                );
            }
        }
    }

    #[test]
    fn reset_backups_are_unique_private_and_previous_backups_remain_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint's data.sqlite3");
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(request(1, 1, 100)).unwrap();
            db.tick().unwrap();
        }
        let mut first = Vec::new();
        reset(&path, &mut first).unwrap();
        let first = std::path::PathBuf::from(String::from_utf8(first).unwrap().trim());
        let original_backup = std::fs::read(&first).unwrap();
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(request(2, 2, 100)).unwrap();
            db.tick().unwrap();
        }
        let mut second = Vec::new();
        reset(&path, &mut second).unwrap();
        let second = std::path::PathBuf::from(String::from_utf8(second).unwrap().trim());
        assert_ne!(first, second);
        assert!(second.is_file());
        assert_eq!(std::fs::read(&first).unwrap(), original_backup);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn reset_refuses_a_running_owner_without_a_backup_or_any_data_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(request(1, 1, 100)).unwrap();
            db.tick().unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        let db = Database::open(&path).unwrap();
        let mut output = Vec::new();
        assert!(reset(&path, &mut output).is_err());
        assert!(output.is_empty());
        assert_eq!(db.snapshot(true).unwrap().height, 1);
        drop(db);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn reset_refuses_missing_corrupt_empty_foreign_and_multiline_database_paths() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.sqlite3");
        let mut output = Vec::new();
        assert!(reset(&missing, &mut output).is_err());
        assert!(!missing.exists());
        for (name, bytes) in [
            ("corrupt.sqlite3", b"not sqlite".as_slice()),
            ("empty.sqlite3", b"".as_slice()),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(reset(&path, &mut output).is_err());
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        let foreign = directory.path().join("foreign.sqlite3");
        Connection::open(&foreign)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
            .unwrap();
        let foreign_bytes = std::fs::read(&foreign).unwrap();
        assert!(reset(&foreign, &mut output).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), foreign_bytes);
        let multiline = directory.path().join("mint\n.sqlite3");
        drop(Database::open(&multiline).unwrap());
        let multiline_bytes = std::fs::read(&multiline).unwrap();
        assert!(reset(&multiline, &mut output).is_err());
        assert_eq!(std::fs::read(&multiline).unwrap(), multiline_bytes);
        assert!(output.is_empty());
        assert!(!std::fs::read_dir(directory.path())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".backup-")));
    }

    fn minter_request(seed: u8, name: Option<&str>) -> MinterRequest {
        MinterRequest {
            address: address(seed),
            name: name.map(str::to_owned),
            service_commission_bps: None,
        }
    }

    fn service_request(seed: u8, nonce: u8) -> ContributionRequest {
        let mut value = request(seed, nonce, 100);
        value.mode = Mode::Service;
        value.duration = 144;
        value.budget_sats = 144 * 1_020;
        value
    }

    fn set_commission(db: &mut Database, seed: u8, rate: i64) -> Result<Minter, Error> {
        let mut profile = minter_request(seed, None);
        profile.service_commission_bps = Some(rate);
        db.minter(profile)
    }

    fn delegated_request(seed: u8, nonce: u8, service: i64) -> ContributionRequest {
        let mut value = request(seed, nonce, 100);
        value.mode = Mode::Delegating;
        value.service_user_id = Some(service);
        value.duration = 144;
        value.budget_sats = 144 * 20;
        value
    }

    #[test]
    fn core_blocks_list_only_eligible_burns_in_contribution_order() {
        let mut db = database();
        let mut late = service_request(1, 1);
        late.delay_blocks = 1;
        let service = db.submit(late).unwrap();
        db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.submit(request(3, 3, 200)).unwrap();
        assert_eq!(db.core_tip().unwrap().height, 0);
        db.tick().unwrap();
        db.tick().unwrap();

        let tip = db.core_tip().unwrap();
        assert_eq!((tip.height, tip.time), (2, CALENDAR_START + BLOCK_SECONDS));
        let blocks = db.core_blocks(1, 100).unwrap();
        let addresses = |block: &CoreBlock| -> Vec<String> {
            block
                .burns
                .iter()
                .map(|burn| burn.address.clone())
                .collect()
        };
        assert_eq!(blocks.len(), 2);
        // Block 1: the service has not started, so its delegate is no weight.
        assert_eq!(addresses(&blocks[0]), vec![address(3)]);
        assert_eq!(
            addresses(&blocks[1]),
            vec![address(1), address(2), address(3)]
        );
        let burned: i64 = db
            .connection
            .query_row(
                "SELECT SUM(burned_sats) FROM accruals WHERE height = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            blocks[1].burns.iter().map(|burn| burn.sats).sum::<i64>(),
            burned
        );
        assert!(db.core_blocks(3, 9).unwrap().is_empty());
        assert!(matches!(db.core_blocks(0, 1), Err(Error::Invalid(_))));
        assert!(matches!(db.core_blocks(2, 1), Err(Error::Invalid(_))));
    }

    #[test]
    fn delegation_shares_fees_and_credits_exact_net_rewards_and_commissions() {
        let mut db = database();
        let service = db.submit(service_request(1, 1)).unwrap();
        db.minter(minter_request(1, Some("Service"))).unwrap();
        let delegated = db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.tick().unwrap();
        let state = db.snapshot(false).unwrap();
        assert_eq!(state.totals.burned_sats, "40");
        assert_eq!(state.totals.fees_sats, "1000");
        assert_eq!(state.totals.reward_sparks, "5000000000");
        let contribution = &state.contributions[0];
        assert_eq!(contribution.id, delegated.id);
        assert_eq!(contribution.mode, "delegating");
        assert_eq!(contribution.fees_sats, "0");
        assert_eq!(contribution.burned_sats, "20");
        assert_eq!(contribution.reward_sparks, "2450000000");
        assert_eq!(contribution.commission_sparks, "50000000");
        assert_eq!(contribution.commission_bps, 200);
        assert_eq!(contribution.minter_id, service.user_id);
        assert_eq!(contribution.minter_name, "Service");
        assert_eq!(state.minters[0].reward_sparks, "2550000000");
        assert_eq!(state.minters[0].burned_sats, "20");
        assert_eq!(state.minters[0].fees_sats, "1000");
        assert_eq!(state.minters[1].reward_sparks, "2450000000");
        assert_eq!(
            state
                .minters
                .iter()
                .map(|m| m.reward_sparks.parse::<i64>().unwrap())
                .sum::<i64>(),
            BLOCK_REWARD_SPARKS
        );
        assert_eq!(state.services[0].commission_bps, 200);
        assert!(state.services[0].active);
        let ledger: (i64, i64) = db
            .connection
            .query_row(
                "SELECT SUM(reward_sparks), SUM(commission_sparks) FROM accruals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ledger, (BLOCK_REWARD_SPARKS, 50_000_000));
        let schedule = &state.chart_contributions.unwrap()[0];
        assert_eq!(
            (
                schedule.mode.as_str(),
                schedule.minter_id,
                schedule.commission_bps
            ),
            ("delegating", service.user_id, 200)
        );
    }

    #[test]
    fn inactive_service_loses_delegated_burn_without_fees_or_rewards_and_can_resume() {
        let mut db = database();
        let mut first = service_request(1, 1);
        first.delay_blocks = 2;
        let service = db.submit(first).unwrap();
        let mut delegated = delegated_request(2, 2, service.user_id);
        delegated.duration = 300;
        delegated.budget_sats = 6_000;
        let delegate_id = db.submit(delegated.clone()).unwrap().id;
        for height in 1..=300 {
            db.tick().unwrap();
            let eligible = (3..147).contains(&height) || (150..294).contains(&height);
            let ledger: (i64,i64,i64) = db.connection.query_row(
                "SELECT fees_sats,reward_sparks,commission_sparks FROM accruals WHERE height=?1 AND contribution_id=?2",
                params![height,delegate_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
            ).unwrap();
            assert_eq!(
                ledger,
                if eligible {
                    (0, 2_500_000_000, 50_000_000)
                } else {
                    (0, 0, 0)
                }
            );
            if height == 147 {
                let inactive = db.snapshot(false).unwrap();
                assert!(!inactive.services[0].active);
                let mut resumed = service_request(1, 3);
                resumed.delay_blocks = 2;
                assert_eq!(db.submit(resumed).unwrap().start_block, 150);
                assert_eq!(db.submit(delegated.clone()).unwrap().id, delegate_id);
            }
        }
        let state = db.snapshot(true).unwrap();
        assert_eq!(state.totals.burned_sats, "11760");
        assert_eq!(state.totals.fees_sats, "288000");
        assert_eq!(
            state.totals.reward_sparks,
            (288 * BLOCK_REWARD_SPARKS).to_string()
        );
        assert_eq!(state.services[0].commission_bps, 200);
        assert!(!state.services[0].active);
        assert_eq!(
            state
                .contributions
                .iter()
                .find(|c| c.id == delegate_id)
                .unwrap()
                .commission_bps,
            200
        );
        assert_eq!(db.submit(delegated).unwrap().id, delegate_id);
        let future = delegated_request(3, 4, service.user_id);
        assert_eq!(db.submit(future).unwrap().start_block, 301);
        db.tick().unwrap();
        let lost = db
            .connection
            .query_row(
                "SELECT burned_sats,fees_sats,reward_sparks FROM blocks WHERE height=301",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(lost, (20, 0, 0));
    }

    #[test]
    fn service_identity_rate_applies_to_all_contributions_and_old_quotes_only_work_on_retries() {
        let mut db = database();
        let original_service = service_request(1, 1);
        let service = db.submit(original_service.clone()).unwrap();
        let mut delegated = delegated_request(2, 2, service.user_id);
        let accepted = db.submit(delegated.clone()).unwrap();
        set_commission(&mut db, 1, 500).unwrap();
        let mut changed = service_request(1, 3);
        changed.delay_blocks = 1000;
        db.submit(changed).unwrap();
        assert_eq!(db.submit(original_service).unwrap().id, service.id);
        assert_eq!(db.snapshot(false).unwrap().services[0].commission_bps, 500);
        assert_eq!(db.submit(delegated.clone()).unwrap().id, accepted.id);
        delegated.commission_bps = Some(200);
        assert_eq!(db.submit(delegated.clone()).unwrap().id, accepted.id);
        delegated.commission_bps = Some(500);
        assert_eq!(db.submit(delegated.clone()).unwrap().id, accepted.id);
        delegated.request_id = "new-delegation-0001".into();
        delegated.commission_bps = Some(200);
        assert!(matches!(
            db.submit(delegated.clone()),
            Err(Error::Invalid(_))
        ));
        delegated.commission_bps = None;
        let new = db.submit(delegated).unwrap();
        assert_eq!(
            db.snapshot(false)
                .unwrap()
                .contributions
                .iter()
                .find(|c| c.id == new.id)
                .unwrap()
                .commission_bps,
            500
        );
        for mut invalid in [
            request(3, 4, 100),
            service_request(3, 5),
            delegated_request(3, 6, 999),
        ] {
            if invalid.mode == Mode::Independent {
                invalid.commission_bps = Some(0);
            }
            if invalid.mode == Mode::Service {
                invalid.service_user_id = Some(service.user_id);
            }
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        for rate in [-1, 10_001, i64::MAX] {
            assert!(matches!(
                set_commission(&mut db, 3, rate),
                Err(Error::Invalid(_))
            ));
            let mut invalid = service_request(3, 7);
            invalid.commission_bps = Some(rate);
            assert!(matches!(db.submit(invalid.clone()), Err(Error::Invalid(_))));
            invalid.mode = Mode::Delegating;
            invalid.service_user_id = Some(service.user_id);
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        let mut chain = delegated_request(3, 8, accepted.user_id);
        assert!(matches!(db.submit(chain.clone()), Err(Error::Invalid(_))));
        chain.service_user_id = None;
        assert!(matches!(db.submit(chain), Err(Error::Invalid(_))));
    }

    fn accrual_terms(db: &Database, contribution_id: i64) -> Vec<(i64, i64, i64)> {
        db.connection
            .prepare(
                "SELECT reward_sparks, commission_sparks, commission_bps FROM accruals
                 WHERE contribution_id = ?1 ORDER BY height",
            )
            .unwrap()
            .query_map([contribution_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn identity_rate_applies_from_the_next_block_and_never_reprices_history() {
        let mut db = database();
        let first = set_commission(&mut db, 1, 200).unwrap();
        assert_eq!(first.service_commission_bps, Some(200));
        assert!(!db.snapshot(false).unwrap().services[0].active);
        let service = db.submit(service_request(1, 1)).unwrap();
        let delegated = db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        let mut future = delegated_request(3, 3, service.user_id);
        future.delay_blocks = 2;
        let future = db.submit(future).unwrap();
        db.tick().unwrap();
        db.tick().unwrap();

        let rewards = db.rewards().unwrap();
        let totals = serde_json::to_value(db.snapshot(true).unwrap().totals).unwrap();
        set_commission(&mut db, 1, 5_000).unwrap();
        // Saving changes no balance and no committed accrual.
        assert_eq!(
            serde_json::to_value(db.rewards().unwrap()).unwrap(),
            serde_json::to_value(&rewards).unwrap()
        );
        let state = db.snapshot(true).unwrap();
        assert_eq!(serde_json::to_value(&state.totals).unwrap(), totals);
        // Contributions show the rate their next blocks will pay.
        for id in [delegated.id, future.id] {
            let contribution = state.contributions.iter().find(|c| c.id == id).unwrap();
            assert_eq!(contribution.commission_bps, 5_000);
        }

        db.tick().unwrap();
        let terms = accrual_terms(&db, delegated.id);
        assert_eq!(
            terms.iter().map(|t| t.2).collect::<Vec<_>>(),
            vec![200, 200, 5_000]
        );
        for (gross, commission, rate) in &terms {
            assert_eq!(*commission, gross * rate / 10_000);
        }
        // The delayed delegation started after the save, so it never paid 2%.
        assert_eq!(
            accrual_terms(&db, future.id)
                .iter()
                .map(|t| t.2)
                .collect::<Vec<_>>(),
            vec![5_000]
        );
        let blocks = db.core_blocks(1, 3).unwrap();
        let rates: Vec<Vec<i64>> = blocks
            .iter()
            .map(|block| {
                block
                    .burns
                    .iter()
                    .filter_map(|burn| burn.service.as_ref())
                    .map(|service| {
                        assert_eq!(service.address, address(1));
                        service.commission_bps
                    })
                    .collect()
            })
            .collect();
        assert_eq!(rates, vec![vec![200], vec![200], vec![5_000, 5_000]]);
        assert_eq!(
            db.rewards()
                .unwrap()
                .iter()
                .map(|r| r.reward_sparks.parse::<i64>().unwrap())
                .sum::<i64>(),
            3 * BLOCK_REWARD_SPARKS
        );

        // Whole-share and zero commissions apply the same way.
        set_commission(&mut db, 1, 10_000).unwrap();
        db.tick().unwrap();
        set_commission(&mut db, 1, 0).unwrap();
        db.tick().unwrap();
        let terms = accrual_terms(&db, delegated.id);
        assert_eq!(terms[3].1, terms[3].0);
        assert_eq!((terms[4].1, terms[4].2), (0, 0));
        let loaded = db.minter(minter_request(1, Some("Renamed"))).unwrap();
        assert_eq!(loaded.service_commission_bps, Some(0));
    }

    #[test]
    fn commission_rounds_each_accrual_and_self_delegation_keeps_its_gross_share() {
        let mut db = database();
        set_commission(&mut db, 1, 333).unwrap();
        let service = db.submit(service_request(1, 1)).unwrap();
        let delegated = db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.submit(request(3, 3, 100)).unwrap();
        for _ in 0..10 {
            db.tick().unwrap();
        }
        let terms = accrual_terms(&db, delegated.id);
        let gross: i64 = terms.iter().map(|t| t.0).sum();
        let expected: i64 = terms
            .iter()
            .map(|t| (i128::from(t.0) * 333 / 10_000) as i64)
            .sum();
        assert_ne!(expected, (i128::from(gross) * 333 / 10_000) as i64);
        let state = db.snapshot(true).unwrap();
        let contribution = state
            .contributions
            .iter()
            .find(|c| c.id == delegated.id)
            .unwrap();
        assert_eq!(contribution.commission_sparks, expected.to_string());
        assert_eq!(contribution.reward_sparks, (gross - expected).to_string());

        let mut db = database();
        let service = db.submit(service_request(1, 1)).unwrap();
        db.submit(delegated_request(1, 2, service.user_id)).unwrap();
        for rate in [0, 10_000, 333] {
            set_commission(&mut db, 1, rate).unwrap();
            db.tick().unwrap();
        }
        assert_eq!(
            db.rewards().unwrap()[0].reward_sparks,
            (3 * BLOCK_REWARD_SPARKS).to_string()
        );
    }

    #[test]
    fn saving_a_rate_touches_no_balance_even_at_the_integer_limit() {
        let mut db = database();
        let service = db.submit(service_request(1, 1)).unwrap();
        db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.tick().unwrap();
        for (balance, rate) in [(i64::MAX, 10_000), (0, 0)] {
            db.connection
                .execute(
                    "UPDATE accounts SET reward_sparks=?1 WHERE id=?2",
                    params![balance, service.user_id],
                )
                .unwrap();
            let mut profile = minter_request(1, Some("Saved"));
            profile.service_commission_bps = Some(rate);
            assert_eq!(
                db.minter(profile).unwrap().service_commission_bps,
                Some(rate)
            );
            assert_eq!(db.rewards().unwrap()[0].reward_sparks, balance.to_string());
        }
    }

    #[test]
    fn version_two_quotes_migrate_to_current_identity_rate_preserving_gross_ledger_and_retries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let original_service = service_request(1, 1);
        let first = delegated_request(2, 2, 1);
        let second = delegated_request(3, 3, 1);
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(original_service.clone()).unwrap();
            db.submit(first.clone()).unwrap();
            db.submit(second.clone()).unwrap();
            db.tick().unwrap();
            db.connection.execute_batch(
                "ALTER TABLE contributions ADD COLUMN commission_bps INTEGER NOT NULL DEFAULT 0;
                 UPDATE contributions SET commission_bps = CASE id WHEN 1 THEN 200 WHEN 2 THEN 0 ELSE 10000 END;
                 UPDATE accruals SET commission_sparks = CASE contribution_id WHEN 3 THEN reward_sparks ELSE 0 END;
                 UPDATE accounts SET reward_sparks =
                     COALESCE((SELECT SUM(a.reward_sparks-a.commission_sparks) FROM accruals a JOIN contributions c
                         ON c.id=a.contribution_id WHERE c.user_id=accounts.id),0)
                     + COALESCE((SELECT SUM(a.commission_sparks) FROM accruals a JOIN contributions c
                         ON c.id=a.contribution_id WHERE c.service_user_id=accounts.id),0);
                 UPDATE accounts SET service_commission_bps=500 WHERE id=1;
                 PRAGMA user_version=2;",
            ).unwrap();
        }
        let snapshot;
        {
            let mut db = Database::open(&path).unwrap();
            let state = db.snapshot(true).unwrap();
            assert_eq!(state.height, 1);
            assert_eq!(state.totals.burned_sats, "60");
            assert_eq!(state.totals.fees_sats, "1000");
            assert_eq!(state.totals.reward_sparks, "5000000000");
            assert_eq!(state.minters[0].reward_sparks, "1833333333");
            assert_eq!(state.minters[1].reward_sparks, "1583333334");
            assert_eq!(state.minters[2].reward_sparks, "1583333333");
            assert!(state.contributions.iter().all(|c| c.commission_bps == 500));
            assert_eq!(state.contributions[0].commission_sparks, "83333333");
            assert_eq!(state.contributions[1].commission_sparks, "83333333");
            // Version 4 records the rate those commissions were priced at.
            for id in [2, 3] {
                assert_eq!(accrual_terms(&db, id)[0].2, 500);
            }
            assert_eq!(accrual_terms(&db, 1)[0].2, 0);
            for (mut retry, quote, id) in [
                (original_service.clone(), 200, 1),
                (first.clone(), 0, 2),
                (second.clone(), 10000, 3),
            ] {
                retry.commission_bps = Some(quote);
                assert_eq!(db.submit(retry).unwrap().id, id);
            }
            assert_eq!(
                db.minter(minter_request(1, None))
                    .unwrap()
                    .service_commission_bps,
                Some(500)
            );
            let has_quote:bool=db.connection.query_row("SELECT COUNT(*)>0 FROM pragma_table_info('contributions') WHERE name='commission_bps'",[],|row|row.get(0)).unwrap();
            assert!(!has_quote);
            assert!(!db
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
            snapshot = serde_json::to_value(state).unwrap();
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(db.snapshot(true).unwrap()).unwrap(),
            snapshot
        );
        assert_eq!(db.submit(first).unwrap().id, 2);
        assert_eq!(db.submit(second).unwrap().id, 3);
        let mut newer = original_service;
        newer.request_id = "new-service-keeps-rate-0001".into();
        db.submit(newer).unwrap();
        assert_eq!(db.snapshot(true).unwrap().services[0].commission_bps, 500);
    }

    #[test]
    fn zero_full_and_self_commission_conserve_every_spark() {
        for rate in [0, 10_000] {
            let mut db = database();
            set_commission(&mut db, 1, rate).unwrap();
            let service = db.submit(service_request(1, 1)).unwrap();
            db.submit(delegated_request(2, 2, service.user_id)).unwrap();
            db.tick().unwrap();
            let state = db.snapshot(true).unwrap();
            assert_eq!(
                state.contributions[0].commission_sparks,
                (2_500_000_000i64 * rate / 10_000).to_string()
            );
            assert_eq!(
                state.contributions[0].reward_sparks,
                (2_500_000_000i64 * (10_000 - rate) / 10_000).to_string()
            );
            assert_eq!(
                state
                    .minters
                    .iter()
                    .map(|m| m.reward_sparks.parse::<i64>().unwrap())
                    .sum::<i64>(),
                BLOCK_REWARD_SPARKS
            );
        }
        let mut db = database();
        set_commission(&mut db, 1, 10_000).unwrap();
        let service = db.submit(service_request(1, 1)).unwrap();
        db.submit(delegated_request(1, 2, service.user_id)).unwrap();
        db.tick().unwrap();
        let state = db.snapshot(true).unwrap();
        assert_eq!(state.minters.len(), 1);
        assert_eq!(
            state.minters[0].reward_sparks,
            BLOCK_REWARD_SPARKS.to_string()
        );
        assert_eq!(state.minters[0].contribution_count, 2);
        assert_eq!(state.totals.fees_sats, "1000");
    }

    #[test]
    fn commission_credit_overflow_rolls_back_owner_rewards_and_entire_block() {
        let mut db = database();
        let service = db.submit(service_request(1, 1)).unwrap();
        db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.connection
            .execute(
                "UPDATE accounts SET reward_sparks=?1 WHERE id=?2",
                params![i64::MAX - 2_500_000_000, service.user_id],
            )
            .unwrap();
        assert!(matches!(db.tick(), Err(Error::Storage(_))));
        let state = db.snapshot(true).unwrap();
        assert_eq!(state.height, 0);
        assert_eq!(state.totals.reward_sparks, "0");
        assert!(state
            .contributions
            .iter()
            .all(|c| c.reward_sparks == "0" && c.commission_sparks == "0"));
        assert_eq!(state.minters[1].reward_sparks, "0");
        let count: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM accruals", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn version_one_ledger_migrates_and_delegation_terms_and_balances_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let mut legacy = request(1, 1, 100);
        legacy.duration = 100;
        legacy.budget_sats = 100_100;
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(
                "CREATE TABLE accounts(id INTEGER PRIMARY KEY, address TEXT NOT NULL UNIQUE, name TEXT,
                     reward_sparks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(reward_sparks)='integer' AND reward_sparks>=0));
                 CREATE TABLE contributions(id INTEGER PRIMARY KEY, request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id), budget_sats INTEGER NOT NULL,
                     duration INTEGER NOT NULL, delay_blocks INTEGER NOT NULL DEFAULT 0, start_block INTEGER NOT NULL,
                     CHECK(budget_sats>=duration*1001));
                 CREATE TABLE blocks(height INTEGER PRIMARY KEY, burned_sats INTEGER NOT NULL, fees_sats INTEGER NOT NULL,
                     reward_sparks INTEGER NOT NULL CHECK((burned_sats=0 AND reward_sparks=0)
                         OR (burned_sats>0 AND reward_sparks=5000000000)));
                 CREATE TABLE accruals(height INTEGER NOT NULL REFERENCES blocks(height),
                     contribution_id INTEGER NOT NULL REFERENCES contributions(id), burned_sats INTEGER NOT NULL,
                     fees_sats INTEGER NOT NULL CHECK(fees_sats=1000), reward_sparks INTEGER NOT NULL,
                     PRIMARY KEY(height,contribution_id));
                 INSERT INTO blocks VALUES(1,1,1000,5000000000);
                 PRAGMA user_version=1;",
            ).unwrap();
            connection
                .execute(
                    "INSERT INTO accounts VALUES(7,?1,'Alice',5000000000)",
                    [&legacy.address],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO contributions VALUES(42,?1,7,100100,100,0,1)",
                    [&legacy.request_id],
                )
                .unwrap();
            connection
                .execute("INSERT INTO accruals VALUES(1,42,1,1000,5000000000)", [])
                .unwrap();
        }
        let delegated;
        let snapshot;
        let rewards;
        {
            let mut db = Database::open(&path).unwrap();
            let state = db.snapshot(true).unwrap();
            assert_eq!(state.contributions[0].id, 42);
            assert_eq!(state.contributions[0].user_id, 7);
            assert_eq!(state.contributions[0].mode, "independent");
            assert_eq!(state.contributions[0].commission_sparks, "0");
            assert_eq!(state.contributions[0].reward_sparks, "5000000000");
            assert_eq!(state.contributions[0].user_name, "Alice");
            assert_eq!(db.submit(legacy.clone()).unwrap().id, 42);
            set_commission(&mut db, 2, 333).unwrap();
            let service = db.submit(service_request(2, 2)).unwrap();
            assert_eq!((service.id, service.user_id), (43, 8));
            delegated = delegated_request(3, 3, service.user_id);
            assert_eq!(db.submit(delegated.clone()).unwrap().id, 44);
            db.tick().unwrap();
            snapshot = serde_json::to_value(db.snapshot(true).unwrap()).unwrap();
            rewards = serde_json::to_value(db.rewards().unwrap()).unwrap();
            assert!(!db
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(db.snapshot(true).unwrap()).unwrap(),
            snapshot
        );
        assert_eq!(
            serde_json::to_value(db.rewards().unwrap()).unwrap(),
            rewards
        );
        assert_eq!(db.submit(delegated).unwrap().id, 44);
        assert_eq!(db.submit(legacy).unwrap().id, 42);
        assert_eq!(db.snapshot(true).unwrap().services[0].commission_bps, 333);
        assert_eq!(
            db.snapshot(true).unwrap().contributions[0].commission_bps,
            333
        );
        assert_eq!(
            db.rewards()
                .unwrap()
                .iter()
                .map(|r| r.reward_sparks.parse::<i64>().unwrap())
                .sum::<i64>(),
            2 * BLOCK_REWARD_SPARKS
        );
    }

    #[test]
    fn inactive_delegation_does_not_dilute_independent_rewards_or_pay_commission() {
        let mut db = database();
        let mut future = service_request(1, 1);
        future.delay_blocks = 1000;
        let service = db.submit(future).unwrap();
        db.submit(delegated_request(2, 2, service.user_id)).unwrap();
        db.submit(request(3, 3, 100)).unwrap();
        db.tick().unwrap();
        let state = db.snapshot(true).unwrap();
        assert_eq!(state.totals.burned_sats, "21");
        assert_eq!(state.totals.fees_sats, "1000");
        assert_eq!(state.totals.reward_sparks, BLOCK_REWARD_SPARKS.to_string());
        assert_eq!(
            state.contributions[0].reward_sparks,
            BLOCK_REWARD_SPARKS.to_string()
        );
        assert_eq!(state.contributions[1].reward_sparks, "0");
        assert_eq!(state.contributions[1].commission_sparks, "0");
        assert!(!state.services[0].active);
    }

    #[test]
    fn minter_profiles_share_existing_incremental_account_ids() {
        let mut db = database();
        let first = db.minter(minter_request(1, None)).unwrap();
        assert_eq!((first.user_id, first.name.as_str()), (1, "Minter 1"));
        let mut uppercase = minter_request(1, None);
        uppercase.address.make_ascii_uppercase();
        assert_eq!(db.minter(uppercase).unwrap().user_id, 1);
        assert_eq!(db.snapshot(true).unwrap().totals.contribution_count, 0);
        assert!(db.snapshot(true).unwrap().contributions.is_empty());
        let existing = db.submit(request(2, 1, 100)).unwrap();
        assert_eq!(existing.user_id, 2);
        let second = db.minter(minter_request(2, None)).unwrap();
        assert_eq!((second.user_id, second.name.as_str()), (2, "Minter 2"));
        let renamed = db.minter(minter_request(1, Some("  Alice  "))).unwrap();
        assert_eq!((renamed.user_id, renamed.name.as_str()), (1, "Alice"));
        assert_eq!(db.minter(minter_request(1, None)).unwrap().name, "Alice");
        assert_eq!(
            db.minter(minter_request(3, Some("Alice"))).unwrap().user_id,
            3
        );
        let original = request(1, 2, 100);
        let submitted = db.submit(original.clone()).unwrap();
        assert_eq!((submitted.id, submitted.user_id), (2, 1));
        db.tick().unwrap();
        let mut snapshot = serde_json::to_value(db.snapshot(true).unwrap()).unwrap();
        let rewards = serde_json::to_value(db.rewards().unwrap()).unwrap();
        db.minter(minter_request(1, Some("Bob"))).unwrap();
        snapshot["contributions"][0]["user_name"] = "Bob".into();
        snapshot["contributions"][0]["minter_name"] = "Bob".into();
        snapshot["minters"][0]["name"] = "Bob".into();
        assert_eq!(db.submit(original).unwrap().user_id, 1);
        assert_eq!(
            serde_json::to_value(db.snapshot(true).unwrap()).unwrap(),
            snapshot
        );
        assert_eq!(
            serde_json::to_value(db.rewards().unwrap()).unwrap(),
            rewards
        );
    }

    #[test]
    fn minter_names_are_trimmed_unicode_text_with_strict_length_and_control_checks() {
        let mut db = database();
        for name in [
            "".to_owned(),
            "   ".to_owned(),
            "x".repeat(65),
            "🔥".repeat(65),
            "Al\0ice".to_owned(),
            "Alice\n".to_owned(),
            "\tAlice".to_owned(),
            "Al\u{7f}ice".to_owned(),
            "Al\u{85}ice".to_owned(),
        ] {
            assert!(matches!(
                db.minter(minter_request(1, Some(&name))),
                Err(Error::Invalid(_))
            ));
        }
        assert!(db.rewards().unwrap().is_empty());
        for name in [
            "  身🔥  ".to_owned(),
            "🔥".repeat(64),
            "x".repeat(64),
            "<script>alert('x')</script>".to_owned(),
            "x'); DROP TABLE accounts; --".to_owned(),
        ] {
            let minter = db.minter(minter_request(1, Some(&name))).unwrap();
            assert_eq!(minter.name, name.trim());
            assert_eq!(
                db.minter(minter_request(1, None)).unwrap().name,
                name.trim()
            );
        }
        assert_eq!(db.rewards().unwrap().len(), 1);
        assert!(matches!(
            db.minter(MinterRequest {
                address: "not-an-address".into(),
                name: None,
                service_commission_bps: None,
            }),
            Err(Error::Invalid(_))
        ));
        let mainnet = ReceivingAddress::from_bech32(&address(2), Network::Testnet)
            .unwrap()
            .to_bech32(Network::Mainnet);
        assert!(matches!(
            db.minter(MinterRequest {
                address: mainnet,
                name: None,
                service_commission_bps: None,
            }),
            Err(Error::Invalid(_))
        ));
        assert_eq!(db.rewards().unwrap().len(), 1);
    }

    #[test]
    fn existing_accounts_gain_names_without_changing_ledger_and_names_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let original = request(1, 1, 100);
        let snapshot;
        let rewards;
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(original.clone()).unwrap();
            db.tick().unwrap();
            snapshot = serde_json::to_value(db.snapshot(true).unwrap()).unwrap();
            rewards = serde_json::to_value(db.rewards().unwrap()).unwrap();
            db.connection
                .execute_batch("ALTER TABLE accounts DROP COLUMN name;")
                .unwrap();
        }
        {
            let mut db = Database::open(&path).unwrap();
            assert_eq!(
                serde_json::to_value(db.snapshot(true).unwrap()).unwrap(),
                snapshot
            );
            assert_eq!(
                serde_json::to_value(db.rewards().unwrap()).unwrap(),
                rewards
            );
            let minter = db.minter(minter_request(1, None)).unwrap();
            assert_eq!((minter.user_id, minter.name.as_str()), (1, "Minter 1"));
            assert_eq!(db.submit(original.clone()).unwrap().id, 1);
            db.minter(minter_request(1, Some("Alice 🔥"))).unwrap();
            assert_eq!(db.minter(minter_request(2, None)).unwrap().user_id, 2);
            assert!(!db
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(db.minter(minter_request(1, None)).unwrap().name, "Alice 🔥");
        assert_eq!(db.minter(minter_request(2, None)).unwrap().name, "Minter 2");
        assert_eq!(db.submit(original).unwrap().user_id, 1);
        assert_eq!(db.submit(request(3, 2, 100)).unwrap().user_id, 3);
        assert_eq!(
            db.rewards()
                .unwrap()
                .iter()
                .find(|r| r.address == address(1))
                .unwrap()
                .reward_sparks,
            "5000000000"
        );
    }

    #[test]
    fn validates_budget_duration_address_and_request_id() {
        let mut db = database();
        let valid = request(1, 1, 100);
        let mut invalid = valid.clone();
        invalid.budget_sats -= 1;
        assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        for duration in [i64::MIN, 0, 100, 143, MAX_SAFE_INTEGER + 1, i64::MAX] {
            let mut invalid = valid.clone();
            invalid.duration = duration;
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        for delay in [i64::MIN, -1, MAX_SAFE_INTEGER + 1, i64::MAX] {
            let mut invalid = valid.clone();
            invalid.delay_blocks = delay;
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        for budget in [1_999, MAX_SAFE_INTEGER + 1, i64::MAX] {
            let mut invalid = valid.clone();
            invalid.budget_sats = budget;
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        let mainnet = ReceivingAddress::from_bech32(&valid.address, Network::Testnet)
            .unwrap()
            .to_bech32(Network::Mainnet);
        let mut corrupt = valid.address.clone();
        let last = corrupt.pop().unwrap();
        corrupt.push(if last == 'q' { 'p' } else { 'q' });
        for address in ["not-an-address".into(), mainnet, corrupt] {
            let mut invalid = valid.clone();
            invalid.address = address;
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        for request_id in ["short".into(), "a".repeat(65), "bad________nonce".into()] {
            let mut invalid = valid.clone();
            invalid.request_id = request_id;
            assert!(matches!(db.submit(invalid), Err(Error::Invalid(_))));
        }
        db.submit(valid).unwrap();
        assert_eq!(db.snapshot(true).unwrap().contributions.len(), 1);
        assert!(serde_json::from_str::<ContributionRequest>(
            r#"{"address":"a","budget_sats":2000,"duration":100,"request_id":"abcdefghijklmnop","seed":"secret"}"#
        ).is_err());
    }

    #[test]
    fn accepts_every_block_duration_and_delay_bounds() {
        let mut db = database();
        for (nonce, duration, delay_blocks) in [(1, 144, 0), (2, 145, 1), (3, 4_464, 4_464)] {
            let mut contribution = request(1, nonce, 100);
            contribution.duration = duration;
            contribution.delay_blocks = delay_blocks;
            contribution.budget_sats = duration * 1_001;
            let submitted = db.submit(contribution).unwrap();
            assert_eq!(submitted.start_block, delay_blocks + 1);
        }
        let snapshot = db.snapshot(false).unwrap();
        assert_eq!(snapshot.contributions.len(), 3);
        assert_eq!(snapshot.contributions[0].duration, 4_464);
        assert_eq!(snapshot.contributions[0].delay_blocks, 4_464);
        assert_eq!(snapshot.totals.burned_sats, "0");
    }

    #[test]
    fn manual_values_may_exceed_every_slider_limit() {
        let mut db = database();
        let mut contribution = request(1, 1, 100);
        contribution.budget_sats = 200_000_000;
        contribution.duration = 5_001;
        contribution.delay_blocks = 6_001;
        let submitted = db.submit(contribution.clone()).unwrap();
        assert_eq!(submitted.start_block, 6_002);
        assert_eq!(db.submit(contribution).unwrap().id, submitted.id);
        let snapshot = db.snapshot(false).unwrap();
        assert_eq!(snapshot.contributions[0].budget_sats, 200_000_000);
        assert_eq!(snapshot.contributions[0].duration, 5_001);
        assert_eq!(snapshot.contributions[0].delay_blocks, 6_001);
    }

    #[test]
    fn validates_safe_integer_limits_funding_and_schedule_without_overflow() {
        let mut db = database();
        let mut contribution = request(1, 1, 100);
        contribution.budget_sats = MAX_SAFE_INTEGER;
        contribution.duration = 144;
        contribution.delay_blocks = MAX_SAFE_INTEGER - 145;
        let last = db.submit(contribution.clone()).unwrap();
        assert_eq!(last.start_block + contribution.duration, MAX_SAFE_INTEGER);

        contribution.request_id = "overlong-schedule-0001".into();
        contribution.delay_blocks += 1;
        assert!(matches!(
            db.submit(contribution.clone()),
            Err(Error::Invalid(_))
        ));
        contribution.delay_blocks = 0;
        contribution.duration = MAX_SAFE_INTEGER;
        assert!(matches!(
            db.submit(contribution.clone()),
            Err(Error::Invalid(_))
        ));

        contribution.duration = MAX_SAFE_INTEGER / 1_001;
        let large = db.submit(contribution.clone()).unwrap();
        assert_eq!(large.start_block, 1);
        db.tick().unwrap();
        assert_eq!(db.snapshot(false).unwrap().totals.burned_sats, "2");

        contribution.request_id = "underfunded-large-0001".into();
        contribution.budget_sats = contribution.duration * 1_001 - 1;
        assert!(matches!(
            db.submit(contribution.clone()),
            Err(Error::Invalid(_))
        ));

        contribution.budget_sats = MAX_SAFE_INTEGER;
        contribution.duration = 144;
        contribution.delay_blocks = MAX_SAFE_INTEGER - 145;
        assert!(matches!(
            db.submit(contribution.clone()),
            Err(Error::Invalid(_))
        ));

        db.connection
            .execute_batch("DELETE FROM accruals; DELETE FROM blocks;")
            .unwrap();
        db.connection
            .execute("INSERT INTO blocks VALUES (?1, 0, 0, 0)", [i64::MAX])
            .unwrap();
        contribution.delay_blocks = MAX_SAFE_INTEGER;
        assert!(matches!(db.submit(contribution), Err(Error::Invalid(_))));
    }

    #[test]
    fn delayed_contribution_spends_only_its_scheduled_blocks_and_retries_keep_the_start() {
        let mut db = database();
        db.tick().unwrap();
        let mut contribution = request(1, 1, 100);
        contribution.duration = 145;
        contribution.budget_sats = 145 * 1_002;
        contribution.delay_blocks = 3;
        let submitted = db.submit(contribution.clone()).unwrap();
        assert_eq!(submitted.start_block, 5);
        for _ in 0..3 {
            db.tick().unwrap();
        }
        assert_eq!(db.submit(contribution).unwrap().start_block, 5);
        assert_eq!(db.snapshot(false).unwrap().totals.burned_sats, "0");
        for _ in 0..145 {
            db.tick().unwrap();
        }
        let final_snapshot = db.snapshot(false).unwrap();
        assert_eq!(final_snapshot.height, 149);
        assert_eq!(final_snapshot.totals.burned_sats, "290");
        assert_eq!(final_snapshot.totals.fees_sats, "145000");
        assert_eq!(
            final_snapshot.totals.reward_sparks,
            (145 * BLOCK_REWARD_SPARKS).to_string()
        );
        db.tick().unwrap();
        assert_eq!(
            serde_json::to_value(db.snapshot(false).unwrap().totals).unwrap(),
            serde_json::to_value(final_snapshot.totals).unwrap()
        );
    }

    #[test]
    fn empty_block_rewards_are_lost_before_between_and_after_minters() {
        let mut db = database();
        for _ in 0..3 {
            db.tick().unwrap();
        }
        assert_eq!(db.snapshot(false).unwrap().totals.reward_sparks, "0");
        assert!(db.rewards().unwrap().is_empty());

        let duration = 144;
        let mut first = request(1, 1, 100);
        first.duration = duration;
        first.budget_sats = duration * (FEE_PER_BLOCK + 1);
        first.delay_blocks = 2;
        let first = db.submit(first).unwrap();
        let mut later = request(2, 2, 100);
        later.duration = duration;
        later.budget_sats = duration * (FEE_PER_BLOCK + 1);
        later.delay_blocks = 2 + duration + 3;
        let later = db.submit(later).unwrap();
        assert_eq!((first.start_block, later.start_block), (6, 153));

        let last_height = later.start_block + duration + 2;
        for height in 4..=last_height {
            db.tick().unwrap();
            let active = [first.start_block, later.start_block]
                .iter()
                .any(|start| height >= *start && height < start + duration);
            let issued: i64 = db
                .connection
                .query_row(
                    "SELECT reward_sparks FROM blocks WHERE height = ?1",
                    [height],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                issued,
                if active { BLOCK_REWARD_SPARKS } else { 0 },
                "Block {height} must issue only its own reward, never skipped rewards"
            );
        }
        assert_eq!(
            db.snapshot(false).unwrap().totals.reward_sparks,
            (2 * duration * BLOCK_REWARD_SPARKS).to_string()
        );
        for balance in db.rewards().unwrap() {
            assert_eq!(
                balance.reward_sparks,
                (duration * BLOCK_REWARD_SPARKS).to_string()
            );
        }
        let accruals: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM accruals", [], |row| row.get(0))
            .unwrap();
        assert_eq!(accruals, 2 * duration);
    }

    #[test]
    fn retries_are_idempotent_and_conflicting_reuse_is_rejected() {
        let mut db = database();
        let original = request(1, 1, 100);
        let first = db.submit(original.clone()).unwrap();
        db.tick().unwrap();
        let retry = db.submit(original.clone()).unwrap();
        assert_eq!((first.id, first.start_block), (retry.id, retry.start_block));
        let mut uppercase = original.clone();
        uppercase.address.make_ascii_uppercase();
        assert_eq!(db.submit(uppercase).unwrap().id, first.id);
        assert_eq!(db.rewards().unwrap().len(), 1);
        assert_eq!(db.rewards().unwrap()[0].address, original.address);
        let mut conflict = original.clone();
        conflict.budget_sats += 1;
        assert!(matches!(db.submit(conflict), Err(Error::Conflict(_))));
        let mut conflict = original.clone();
        conflict.duration += 1;
        assert!(matches!(db.submit(conflict), Err(Error::Conflict(_))));
        let mut conflict = original.clone();
        conflict.delay_blocks = 1;
        assert!(matches!(db.submit(conflict), Err(Error::Conflict(_))));
        let mut conflict = original;
        conflict.address = address(2);
        assert!(matches!(db.submit(conflict), Err(Error::Conflict(_))));
        assert_eq!(db.snapshot(false).unwrap().contributions.len(), 1);
    }

    #[test]
    fn user_ids_increment_per_canonical_address_not_per_contribution_or_retry() {
        let mut db = database();
        assert_eq!(db.snapshot(false).unwrap().height, 0);
        assert!(db.rewards().unwrap().is_empty());
        let original = request(1, 1, 100);
        let first = db.submit(original.clone()).unwrap();
        assert_eq!((first.id, first.user_id), (1, 1));
        assert_eq!(db.submit(original.clone()).unwrap().user_id, 1);
        let mut same_user = request(1, 2, 100);
        same_user.address.make_ascii_uppercase();
        let second = db.submit(same_user).unwrap();
        assert_eq!((second.id, second.user_id), (2, 1));
        let mut conflict = original;
        conflict.address = address(2);
        assert!(matches!(db.submit(conflict), Err(Error::Conflict(_))));
        let third = db.submit(request(2, 3, 100)).unwrap();
        assert_eq!((third.id, third.user_id), (3, 2));
        assert_eq!(db.submit(request(3, 4, 100)).unwrap().user_id, 3);
        let snapshot = db.snapshot(true).unwrap();
        let ids: Vec<_> = snapshot
            .contributions
            .iter()
            .rev()
            .map(|contribution| contribution.user_id)
            .collect();
        assert_eq!(ids, [1, 1, 2, 3]);
        for contribution in snapshot.contributions {
            assert_eq!(contribution.address, address(contribution.user_id as u8));
        }
        assert!(db
            .connection
            .execute("UPDATE contributions SET user_id = 999 WHERE id = 1", [])
            .is_err());
    }

    #[test]
    fn contribution_starts_next_block_and_spends_exact_budget() {
        let mut db = database();
        db.tick().unwrap();
        assert_eq!(db.snapshot(false).unwrap().totals.reward_sparks, "0");
        let submitted = db.submit(request(1, 1, 137)).unwrap();
        assert_eq!(submitted.start_block, 2);
        assert_eq!(db.snapshot(true).unwrap().contributions[0].burned_sats, "0");
        for offset in 0..200 {
            db.tick().unwrap();
            let burn: i64 = db
                .connection
                .query_row(
                    "SELECT burned_sats FROM accruals WHERE height = ?1",
                    [offset + 2],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(burn, if offset < 74 { 2 } else { 1 });
        }
        let snapshot = db.snapshot(false).unwrap();
        assert_eq!(snapshot.totals.burned_sats, "274");
        assert_eq!(snapshot.totals.fees_sats, "200000");
        assert_eq!(
            snapshot.totals.reward_sparks,
            (200 * BLOCK_REWARD_SPARKS).to_string()
        );
        db.tick().unwrap();
        assert_eq!(
            db.snapshot(false).unwrap().totals.reward_sparks,
            snapshot.totals.reward_sparks
        );
    }

    #[test]
    fn dashboard_includes_scheduled_contributions_without_premature_accruals() {
        let mut db = database();
        let mut scheduled = request(1, 1, 100);
        scheduled.delay_blocks = 2;
        let submitted = db.submit(scheduled).unwrap();
        let pending = db.snapshot(false).unwrap();
        assert_eq!(pending.height, 0);
        assert_eq!(pending.contributions.len(), 1);
        assert_eq!(pending.contributions[0].delay_blocks, 2);
        assert_eq!(pending.totals.contribution_count, 1);
        assert_eq!(pending.totals.burned_sats, "0");
        assert_eq!(pending.totals.fees_sats, "0");
        assert_eq!(pending.totals.reward_sparks, "0");
        let full = db.snapshot(true).unwrap();
        assert_eq!(full.contributions.len(), 1);
        assert_eq!(full.contributions[0].id, submitted.id);
        assert_eq!(full.contributions[0].start_block, submitted.start_block);
        assert_eq!(
            serde_json::to_value(&full.totals).unwrap(),
            serde_json::to_value(&pending.totals).unwrap()
        );

        for _ in 0..2 {
            db.tick().unwrap();
            assert_eq!(db.snapshot(false).unwrap().totals.burned_sats, "0");
        }
        db.tick().unwrap();
        let started = db.snapshot(false).unwrap();
        assert_eq!(started.height, submitted.start_block);
        assert_eq!(started.contributions.len(), 1);
        assert_eq!(started.contributions[0].id, submitted.id);
        assert_eq!(started.contributions[0].burned_sats, "1");
        assert_eq!(started.totals.contribution_count, 1);
    }

    #[test]
    fn dashboard_includes_old_contributions_without_losing_lifetime_totals() {
        let mut db = database();
        db.submit(request(1, 1, 100)).unwrap();
        db.tick().unwrap();
        db.connection
            .execute(
                "INSERT INTO blocks(height, burned_sats, fees_sats, reward_sparks)
                 VALUES (10201, 0, 0, 0)",
                [],
            )
            .unwrap();
        let boundary = db.submit(request(2, 2, 100)).unwrap();
        db.connection
            .execute(
                "UPDATE contributions SET start_block = 2 WHERE id = ?1",
                [boundary.id],
            )
            .unwrap();
        let active = db.submit(request(3, 3, 100)).unwrap();
        db.connection
            .execute(
                "UPDATE contributions SET start_block = 10102 WHERE id = ?1",
                [active.id],
            )
            .unwrap();
        let future = db.submit(request(4, 4, 100)).unwrap();
        let recent = db.snapshot(false).unwrap();
        assert_eq!(recent.height, 10_201);
        assert_eq!(
            recent
                .contributions
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [future.id, active.id, boundary.id, 1]
        );
        assert_eq!(recent.totals.contribution_count, 4);
        assert_eq!(recent.totals.burned_sats, "1");
        assert_eq!(recent.totals.fees_sats, "1000");
        assert_eq!(recent.totals.reward_sparks, "5000000000");
        let full = db.snapshot(true).unwrap();
        assert_eq!(full.contributions.len(), 4);
        assert_eq!(full.contributions[0].id, future.id);
        assert_eq!(full.contributions[3].id, 1);
        assert_eq!(full.contributions[3].reward_sparks, "5000000000");
        assert_eq!(
            serde_json::to_value(&full.totals).unwrap(),
            serde_json::to_value(&recent.totals).unwrap()
        );
        assert_eq!(
            db.rewards()
                .unwrap()
                .iter()
                .find(|reward| reward.address == address(1))
                .unwrap()
                .reward_sparks,
            "5000000000"
        );
        let record_counts: (i64, i64, i64) = db
            .connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM contributions),
                        (SELECT COUNT(*) FROM blocks), (SELECT COUNT(*) FROM accruals)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(record_counts, (4, 2, 1));
    }

    #[test]
    fn dashboard_limits_details_to_latest_200_ids_but_keeps_every_chart_schedule() {
        let mut db = database();
        let empty = db.snapshot(false).unwrap();
        assert!(empty.contributions.is_empty());
        assert!(empty.chart_contributions.unwrap().is_empty());
        let mut active = request(1, 1, 100);
        active.duration = 50_000;
        active.budget_sats = 100_000_000;
        db.submit(active).unwrap();
        db.tick().unwrap();
        db.connection
            .execute("INSERT INTO blocks VALUES (20001, 0, 0, 0)", [])
            .unwrap();
        for id in 2..=205 {
            db.connection.execute(
                "INSERT INTO contributions(id, request_id, user_id, budget_sats, duration, start_block)
                 VALUES (?1, ?2, 1, 400000, 200, ?3)",
                params![id, format!("bulk-contribution-{id:016}"), if id == 205 { 1 } else { 20002 }],
            ).unwrap();
            if [199, 200, 205].contains(&id) {
                let recent = db.snapshot(false).unwrap();
                assert_eq!(recent.contributions.len(), id.min(200) as usize);
                assert_eq!(recent.contributions[0].id, id);
                assert_eq!(recent.contributions.last().unwrap().id, (id - 199).max(1));
                assert_eq!(recent.chart_contributions.unwrap().len(), id as usize);
                assert_eq!(recent.totals.contribution_count, id);
                assert_eq!(recent.totals.burned_sats, "1000");
                assert_eq!(recent.totals.fees_sats, "1000");
                assert_eq!(recent.totals.reward_sparks, "5000000000");
            }
        }
        let recent = db.snapshot(false).unwrap();
        assert_eq!(recent.contributions[0].start_block, 1);
        assert!(recent.contributions.iter().all(|c| c.id != 1));
        let schedules = recent.chart_contributions.unwrap();
        let oldest = schedules.last().unwrap();
        assert_eq!(
            (oldest.id, oldest.start_block, oldest.duration),
            (1, 1, 50_000)
        );
        assert!(
            oldest.start_block <= recent.height
                && oldest.start_block + oldest.duration > recent.height
        );
        let schedule_json = serde_json::to_value(oldest).unwrap();
        assert_eq!(schedule_json.as_object().unwrap().len(), 9);
        for omitted in [
            "address",
            "burned_sats",
            "fees_sats",
            "reward_sparks",
            "delay_blocks",
        ] {
            assert!(schedule_json.get(omitted).is_none());
        }
        let full = db.snapshot(true).unwrap();
        assert_eq!(full.contributions.len(), 205);
        assert_eq!(full.contributions.last().unwrap().id, 1);
        assert!(full.chart_contributions.is_none());
        assert!(serde_json::to_value(&full)
            .unwrap()
            .get("chart_contributions")
            .is_none());
        assert_eq!(
            full.contributions.last().unwrap().reward_sparks,
            "5000000000"
        );
        assert_eq!(
            serde_json::to_value(&full.totals).unwrap(),
            serde_json::to_value(&recent.totals).unwrap()
        );
        assert_eq!(db.rewards().unwrap()[0].reward_sparks, "5000000000");
        let record_counts: (i64, i64, i64) = db
            .connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM contributions), (SELECT COUNT(*) FROM blocks),
                    (SELECT COUNT(*) FROM accruals)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(record_counts, (205, 2, 1));
    }

    #[test]
    fn proportional_rewards_conserve_every_spark_and_accumulate_by_address() {
        let mut db = database();
        for (seed, nonce, burn) in [(1, 1, 100), (2, 2, 100), (1, 3, 100)] {
            db.submit(request(seed, nonce, burn)).unwrap();
        }
        db.tick().unwrap();
        let snapshot = db.snapshot(false).unwrap();
        assert_eq!(snapshot.totals.burned_sats, "3");
        assert_eq!(snapshot.totals.fees_sats, "3000");
        assert_eq!(snapshot.totals.reward_sparks, "5000000000");
        let by_id: Vec<_> = snapshot
            .contributions
            .iter()
            .rev()
            .map(|c| c.reward_sparks.as_str())
            .collect();
        assert_eq!(by_id, ["1666666667", "1666666667", "1666666666"]);
        let rewards = db.rewards().unwrap();
        assert_eq!(
            rewards
                .iter()
                .find(|item| item.address == address(1))
                .unwrap()
                .reward_sparks,
            "3333333333"
        );
        assert_eq!(
            rewards
                .iter()
                .find(|item| item.address == address(2))
                .unwrap()
                .reward_sparks,
            "1666666667"
        );
        assert_eq!(snapshot.contributions[0].id, 3);
    }

    #[test]
    fn unequal_burn_weights_receive_proportional_rewards() {
        let mut db = database();
        db.submit(request(1, 1, 100)).unwrap();
        db.submit(request(2, 2, 200)).unwrap();
        db.tick().unwrap();
        let contributions = db.snapshot(false).unwrap().contributions;
        assert_eq!(contributions[0].reward_sparks, "3333333333");
        assert_eq!(contributions[1].reward_sparks, "1666666667");
    }

    #[test]
    fn restart_resumes_ledger_without_catching_up() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(request(1, 1, 100)).unwrap();
            db.tick().unwrap();
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(db.snapshot(false).unwrap().height, 1);
        assert_eq!(db.snapshot(false).unwrap().contributions[0].user_id, 1);
        assert_eq!(db.rewards().unwrap()[0].reward_sparks, "5000000000");
        db.tick().unwrap();
        assert_eq!(db.snapshot(false).unwrap().height, 2);
        assert_eq!(db.rewards().unwrap()[0].reward_sparks, "10000000000");
        assert_eq!(db.submit(request(1, 2, 100)).unwrap().user_id, 1);
        assert_eq!(db.submit(request(2, 3, 100)).unwrap().user_id, 2);
    }

    #[test]
    fn legacy_schema_migrates_without_losing_ids_rewards_accruals_or_retries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let mut legacy_request = request(1, 1, 100);
        legacy_request.duration = 100;
        legacy_request.budget_sats = 100_100;
        {
            let db = Database::open(&path).unwrap();
            db.connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                 BEGIN EXCLUSIVE;
                 CREATE TABLE contributions_legacy (
                     id INTEGER PRIMARY KEY,
                     request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id),
                     budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                         AND budget_sats BETWEEN 2000 AND 100000000),
                     duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                         AND duration BETWEEN 100 AND 1000 AND duration % 50 = 0),
                     start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                         AND start_block >= 1),
                     CHECK(budget_sats >= duration * 1001)
                 );
                 DROP TABLE contributions;
                 ALTER TABLE contributions_legacy RENAME TO contributions;
                 PRAGMA user_version = 0;
                 COMMIT;
                 PRAGMA foreign_keys = ON;",
                )
                .unwrap();
            db.connection
                .execute(
                    "INSERT INTO accounts(id, address, reward_sparks) VALUES (7, ?1, 5000000000)",
                    [&legacy_request.address],
                )
                .unwrap();
            db.connection.execute(
                "INSERT INTO contributions(id, request_id, user_id, budget_sats, duration, start_block)
                 VALUES (42, ?1, 7, 100100, 100, 1)",
                [&legacy_request.request_id],
            ).unwrap();
            db.connection
                .execute_batch(
                    "INSERT INTO blocks VALUES (1, 1, 1000, 5000000000);
                 INSERT INTO accruals(height, contribution_id, burned_sats, fees_sats, reward_sparks)
                     VALUES (1, 42, 1, 1000, 5000000000);",
                )
                .unwrap();
        }
        let scheduled;
        {
            let mut db = Database::open(&path).unwrap();
            let snapshot = db.snapshot(false).unwrap();
            assert_eq!(snapshot.height, 1);
            assert_eq!(snapshot.contributions[0].id, 42);
            assert_eq!(snapshot.contributions[0].user_id, 7);
            assert_eq!(snapshot.contributions[0].duration, 100);
            assert_eq!(snapshot.contributions[0].delay_blocks, 0);
            assert_eq!(snapshot.contributions[0].reward_sparks, "5000000000");
            let retry = db.submit(legacy_request.clone()).unwrap();
            assert_eq!((retry.id, retry.user_id, retry.start_block), (42, 7, 1));
            let mut rejected = legacy_request.clone();
            rejected.request_id = "new-legacy-duration-0001".into();
            assert!(matches!(db.submit(rejected), Err(Error::Invalid(_))));
            let mut delayed = request(2, 2, 100);
            delayed.duration = 145;
            delayed.delay_blocks = 4_464;
            scheduled = db.submit(delayed.clone()).unwrap();
            assert_eq!(
                (scheduled.id, scheduled.user_id, scheduled.start_block),
                (43, 8, 4_466)
            );
            db.tick().unwrap();
            assert_eq!(db.submit(legacy_request).unwrap().id, 42);
            assert_eq!(
                db.submit(delayed).unwrap().start_block,
                scheduled.start_block
            );
            assert_eq!(
                db.rewards()
                    .unwrap()
                    .iter()
                    .find(|r| r.address == address(1))
                    .unwrap()
                    .reward_sparks,
                "10000000000"
            );
            assert_eq!(
                db.connection
                    .query_row(
                        "SELECT COUNT(*) FROM accruals WHERE contribution_id = 42",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                2
            );
            assert!(!db
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
            assert!(db
                .connection
                .execute("UPDATE contributions SET user_id = 999 WHERE id = 42", [])
                .is_err());
        }
        let db = Database::open(&path).unwrap();
        let snapshot = db.snapshot(false).unwrap();
        assert_eq!(snapshot.height, 2);
        assert_eq!(snapshot.contributions[0].id, scheduled.id);
        assert_eq!(snapshot.contributions[0].delay_blocks, 4_464);
        assert_eq!(snapshot.contributions[1].reward_sparks, "10000000000");
        assert_eq!(snapshot.totals.burned_sats, "2");
        assert_eq!(snapshot.totals.reward_sparks, "10000000000");
    }

    #[test]
    fn bounded_schema_migrates_with_ledger_retries_and_delay_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let first = request(1, 1, 100);
        let mut delayed = request(2, 2, 100);
        delayed.delay_blocks = 4_464;
        let snapshot;
        {
            let mut db = Database::open(&path).unwrap();
            db.submit(first.clone()).unwrap();
            db.submit(delayed.clone()).unwrap();
            db.tick().unwrap();
            snapshot = serde_json::to_value(db.snapshot(true).unwrap()).unwrap();
            db.connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                 BEGIN EXCLUSIVE;
                 CREATE TABLE contributions_bounded (
                     id INTEGER PRIMARY KEY,
                     request_id TEXT NOT NULL UNIQUE,
                     user_id INTEGER NOT NULL REFERENCES accounts(id),
                     budget_sats INTEGER NOT NULL CHECK(typeof(budget_sats) = 'integer'
                         AND budget_sats BETWEEN 2000 AND 100000000),
                     duration INTEGER NOT NULL CHECK(typeof(duration) = 'integer'
                         AND duration BETWEEN 100 AND 4464),
                     delay_blocks INTEGER NOT NULL DEFAULT 0 CHECK(typeof(delay_blocks) = 'integer'
                         AND delay_blocks BETWEEN 0 AND 4464),
                     start_block INTEGER NOT NULL CHECK(typeof(start_block) = 'integer'
                         AND start_block >= 1),
                     CHECK(budget_sats >= duration * 1001)
                 );
                 INSERT INTO contributions_bounded
                     SELECT id, request_id, user_id, budget_sats, duration, delay_blocks, start_block FROM contributions;
                 DROP TABLE contributions;
                 ALTER TABLE contributions_bounded RENAME TO contributions;
                 PRAGMA user_version = 0;
                 COMMIT;
                 PRAGMA foreign_keys = ON;",
                )
                .unwrap();
        }
        {
            let mut db = Database::open(&path).unwrap();
            assert_eq!(
                serde_json::to_value(db.snapshot(true).unwrap()).unwrap(),
                snapshot
            );
            assert_eq!(db.submit(first).unwrap().id, 1);
            assert_eq!(db.submit(delayed).unwrap().start_block, 4_465);
            assert_eq!(
                db.rewards()
                    .unwrap()
                    .iter()
                    .find(|r| r.address == address(1))
                    .unwrap()
                    .reward_sparks,
                "5000000000"
            );
            let mut large = request(3, 3, 100);
            large.budget_sats = 200_000_000;
            large.duration = 5_001;
            large.delay_blocks = 6_001;
            let submitted = db.submit(large).unwrap();
            assert_eq!(
                (submitted.id, submitted.user_id, submitted.start_block),
                (3, 3, 6_003)
            );
            assert!(!db
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
            assert!(db
                .connection
                .execute("UPDATE contributions SET user_id = 999 WHERE id = 1", [])
                .is_err());
            db.tick().unwrap();
        }
        let db = Database::open(&path).unwrap();
        assert_eq!(db.snapshot(true).unwrap().height, 2);
        assert_eq!(db.snapshot(true).unwrap().contributions.len(), 3);
        assert_eq!(
            db.snapshot(true).unwrap().totals.reward_sparks,
            "10000000000"
        );
    }

    #[test]
    fn database_file_has_one_owner_until_connection_closes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        let mut first = Database::open(&path).unwrap();
        first.tick().unwrap();
        assert!(matches!(Database::open(&path), Err(Error::Storage(_))));
        assert_eq!(first.snapshot(false).unwrap().height, 1);
        drop(first);
        let second = Database::open(&path).unwrap();
        assert_eq!(second.snapshot(false).unwrap().height, 1);
    }

    #[test]
    fn overflow_rolls_back_entire_tick() {
        let mut db = database();
        db.submit(request(1, 1, 100)).unwrap();
        db.submit(request(2, 2, 100)).unwrap();
        db.connection
            .execute(
                "UPDATE accounts SET reward_sparks = ?1 WHERE address = ?2",
                params![i64::MAX, address(2)],
            )
            .unwrap();
        assert!(matches!(db.tick(), Err(Error::Storage(_))));
        let snapshot = db.snapshot(true).unwrap();
        assert_eq!(snapshot.height, 0);
        assert_eq!(snapshot.totals.reward_sparks, "0");
        assert!(snapshot
            .contributions
            .iter()
            .all(|c| c.reward_sparks == "0"));
        assert_eq!(
            db.rewards()
                .unwrap()
                .iter()
                .find(|r| r.address == address(1))
                .unwrap()
                .reward_sparks,
            "0"
        );
        db.connection
            .execute(
                "UPDATE accounts SET reward_sparks = 0 WHERE address = ?1",
                [address(2)],
            )
            .unwrap();
        db.tick().unwrap();
        assert_eq!(db.snapshot(false).unwrap().height, 1);
    }
}
