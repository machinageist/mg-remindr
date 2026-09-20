// Author: Jeff
// Date: 2026-09-20
// Description: What the embedded schema is allowed to be — one migration, append-only, and
//              exactly the tables it claims
// Notes: The PostgreSQL store grew twelve migrations because each shipped separately. The
//        SQLite store starts from one, and the rules that kept those twelve honest are the
//        rules this one is held to: pinned by checksum, never rewritten, never destructive

use mg_remindr::storage::{AUTHORITY_MIGRATION, MIGRATIONS};
use sha2::{Digest, Sha256};

const HEX: &[u8; 16] = b"0123456789abcdef";

fn sha256_hex(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

#[test]
fn the_authority_migration_is_embedded_and_pinned_by_its_checksum() {
    assert_eq!(MIGRATIONS.len(), 1);
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[0].name, "remindr_authority");
    assert_eq!(MIGRATIONS[0].sql, AUTHORITY_MIGRATION);
    assert_eq!(sha256_hex(AUTHORITY_MIGRATION), MIGRATIONS[0].checksum);
}

#[test]
fn the_migration_creates_every_table_it_claims_and_claims_every_table_it_creates() {
    for migration in MIGRATIONS {
        for table in migration.tables {
            assert!(
                migration.sql.contains(&format!("CREATE TABLE {table}")),
                "migration {} claims {table} without creating it",
                migration.version
            );
        }
        let created = migration.sql.matches("CREATE TABLE ").count();
        assert_eq!(
            created,
            migration.tables.len(),
            "migration {} creates a table its ledger entry does not name",
            migration.version
        );
    }
}

#[test]
fn the_migration_is_append_only_and_never_destructive() {
    // IF NOT EXISTS would let a rewritten migration pass over a store it does not match
    assert!(!AUTHORITY_MIGRATION.contains("IF NOT EXISTS"));
    assert!(!AUTHORITY_MIGRATION.contains("DROP "));
    // `ON DELETE` is part of a reference, so only a statement counts as destructive
    assert!(!AUTHORITY_MIGRATION.contains("DELETE FROM"));
    assert!(!AUTHORITY_MIGRATION.contains("ALTER TABLE"));
    // the only writes are the triggers that bump the authority revision, and they
    // touch nothing but the one row that counts revisions
    for line in AUTHORITY_MIGRATION.lines().map(str::trim_start) {
        if line.starts_with("UPDATE ") {
            assert!(
                line.starts_with("UPDATE mg_remindr_authority_state"),
                "the schema writes to something other than the revision counter: {line}"
            );
        }
    }
}

#[test]
fn the_authority_keeps_the_invariants_the_domain_relies_on() {
    assert!(AUTHORITY_MIGRATION.contains("CHECK (version >= 1)"));
    assert!(AUTHORITY_MIGRATION.contains("CHECK (lifecycle IN ('open', 'completed', 'trashed'))"));
    assert!(AUTHORITY_MIGRATION.contains("CHECK (updated_at >= created_at)"));
    assert!(AUTHORITY_MIGRATION.contains("PRIMARY KEY (todo_id, tag_id)"));
    assert!(AUTHORITY_MIGRATION.contains("ON DELETE CASCADE"));
    assert!(AUTHORITY_MIGRATION.contains("ON DELETE RESTRICT"));
    assert!(AUTHORITY_MIGRATION.contains("CHECK (child_id <> parent_id)"));
    assert!(AUTHORITY_MIGRATION.contains("idempotency_key TEXT NOT NULL UNIQUE"));
}

#[test]
fn migration_versions_are_strictly_increasing() {
    for pair in MIGRATIONS.windows(2) {
        assert!(pair[0].version < pair[1].version);
    }
}
