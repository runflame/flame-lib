use std::{fs, path::Path, process::Command};

use flamekd::{Network, SpendKey};
use rusqlite::{params, Connection};

fn address(seed: u8) -> String {
    SpendKey::from_seed(&[seed; 64])
        .unwrap()
        .to_recv()
        .to_address()
        .to_bech32(Network::Testnet)
}

fn fixture(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE accounts (
                 id INTEGER PRIMARY KEY, address TEXT NOT NULL UNIQUE, name TEXT,
                 reward_sparks INTEGER NOT NULL DEFAULT 0, service_commission_bps INTEGER
             );
             CREATE TABLE contributions (
                 id INTEGER PRIMARY KEY, request_id TEXT NOT NULL UNIQUE,
                 user_id INTEGER NOT NULL REFERENCES accounts(id), budget_sats INTEGER NOT NULL,
                 duration INTEGER NOT NULL, delay_blocks INTEGER NOT NULL DEFAULT 0,
                 start_block INTEGER NOT NULL, mode TEXT NOT NULL DEFAULT 'independent',
                 service_user_id INTEGER REFERENCES accounts(id)
             );
             CREATE TABLE blocks (
                 height INTEGER PRIMARY KEY, burned_sats INTEGER NOT NULL,
                 fees_sats INTEGER NOT NULL, reward_sparks INTEGER NOT NULL
             );
             CREATE TABLE accruals (
                 height INTEGER NOT NULL REFERENCES blocks(height),
                 contribution_id INTEGER NOT NULL REFERENCES contributions(id),
                 burned_sats INTEGER NOT NULL, fees_sats INTEGER NOT NULL,
                 reward_sparks INTEGER NOT NULL, commission_sparks INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(height, contribution_id)
             );
             PRAGMA user_version = 3;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO accounts VALUES (7, ?1, 'Reset fixture', 5000000000, 200)",
            [address(42)],
        )
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO contributions VALUES (8, 'reset-fixture-000001', 7, 300000, 144, 0, 5, 'independent', NULL);
             INSERT INTO blocks VALUES (5, 1084, 1000, 5000000000);
             INSERT INTO accruals VALUES (5, 8, 1084, 1000, 5000000000, 0);",
        )
        .unwrap();
}

fn reset(path: &Path, directory: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mint-playground"))
        .arg("--reset-db")
        .arg(path)
        .current_dir(directory)
        .env("MINT_LISTEN", "not-a-listening-address")
        .output()
        .unwrap()
}

fn backup(output: &std::process::Output, source: &Path) -> std::path::PathBuf {
    assert!(
        output.status.success(),
        "reset failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout contains only the backup path");
    assert!(stdout.ends_with('\n'));
    let backup = std::path::PathBuf::from(lines[0]);
    assert!(backup.is_absolute());
    assert!(backup.is_file());
    assert_eq!(
        backup.parent().unwrap(),
        source.parent().unwrap().canonicalize().unwrap()
    );
    assert_ne!(backup, source);
    backup
}

fn assert_fixture(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let marker: (i64, String, i64, i64) = connection
        .query_row(
            "SELECT id, name, reward_sparks, service_commission_bps FROM accounts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(marker, (7, "Reset fixture".into(), 5_000_000_000, 200));
    let ledger: (i64, i64, i64) = connection
        .query_row(
            "SELECT c.id, b.height, a.reward_sparks FROM accruals a
             JOIN contributions c ON c.id = a.contribution_id JOIN blocks b ON b.height = a.height",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(ledger, (8, 5, 5_000_000_000));
    let integrity: String = connection
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn reset_prints_a_usable_backup_and_clears_the_ledger_from_any_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger with spaces.sqlite3");
    fixture(&path);
    let first_backup = backup(&reset(&path, elsewhere.path()), &path);
    assert_fixture(&first_backup);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&first_backup).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    {
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        for table in ["accounts", "contributions", "blocks", "accruals"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} must be empty");
        }
        connection
            .execute("INSERT INTO accounts(address) VALUES (?1)", [address(43)])
            .unwrap();
        assert_eq!(connection.last_insert_rowid(), 1);
        connection
            .execute(
                "INSERT INTO contributions(request_id, user_id, budget_sats, duration, start_block)
                 VALUES (?1, ?2, 300000, 144, 1)",
                params!["new-reset-fixture-0001", 1],
            )
            .unwrap();
        assert_eq!(connection.last_insert_rowid(), 1);
    }
    let second_backup = backup(&reset(&path, elsewhere.path()), &path);
    assert_ne!(second_backup, first_backup);
    assert_fixture(&first_backup);
}

#[test]
fn missing_database_is_not_created() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.sqlite3");
    let output = reset(&path, directory.path());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn exclusive_owner_prevents_reset_without_changing_or_backing_up_the_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owned.sqlite3");
    fixture(&path);
    // Closing a separate descriptor can release this process's POSIX SQLite locks.
    let before = fs::read(&path).unwrap();
    let owner = Connection::open(&path).unwrap();
    owner
        .execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    let output = reset(&path, directory.path());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    owner.execute_batch("ROLLBACK;").unwrap();
    drop(owner);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_fixture(&path);
}
