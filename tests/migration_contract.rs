use mg_todo::storage::{
    AUTHORITY_REVISION_MIGRATION, FOUNDATION_MIGRATION, MIGRATIONS, TAG_MIGRATION, TODO_MIGRATION,
    TODO_TAG_MIGRATION,
};
use sha2::{Digest, Sha256};

const IMMUTABLE_V1_SHA256: &str =
    "0c821017adbb6c219ad371a7729b7d898a178bd9102279d71524504710ed78c0";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[test]
fn foundation_migrations_are_embedded_and_append_only() {
    assert_eq!(MIGRATIONS.len(), 5);
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[0].name, "project_authority");
    assert_eq!(MIGRATIONS[0].sql, FOUNDATION_MIGRATION);
    assert!(FOUNDATION_MIGRATION.contains("CREATE TABLE projects"));
    assert!(!FOUNDATION_MIGRATION.contains("IF NOT EXISTS"));
    let digest = Sha256::digest(FOUNDATION_MIGRATION.as_bytes());
    let digest = digest
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect::<String>();
    assert_eq!(digest, IMMUTABLE_V1_SHA256);
    assert!(FOUNDATION_MIGRATION.contains("version bigint NOT NULL"));
    assert!(FOUNDATION_MIGRATION.contains("CHECK (version >= 1)"));
    assert!(FOUNDATION_MIGRATION.contains("CHECK (lifecycle IN ('open', 'completed', 'trashed'))"));
    assert!(!FOUNDATION_MIGRATION.contains("DROP TABLE"));
    assert!(!FOUNDATION_MIGRATION.contains("CREATE EXTENSION"));

    assert_eq!(MIGRATIONS[1].version, 2);
    assert_eq!(MIGRATIONS[1].name, "tag_authority");
    assert_eq!(MIGRATIONS[1].sql, TAG_MIGRATION);
    assert!(TAG_MIGRATION.contains("CREATE TABLE tags"));
    assert!(!TAG_MIGRATION.contains("IF NOT EXISTS"));
    assert!(TAG_MIGRATION.contains("version bigint NOT NULL"));
    assert!(TAG_MIGRATION.contains("CHECK (version >= 1)"));
    assert!(TAG_MIGRATION.contains("CHECK (updated_at >= created_at)"));
    assert!(!TAG_MIGRATION.contains("DROP "));
    assert!(!TAG_MIGRATION.contains("ALTER TABLE projects"));
    assert!(!TAG_MIGRATION.contains("CREATE EXTENSION"));

    assert_eq!(MIGRATIONS[2].version, 3);
    assert_eq!(MIGRATIONS[2].name, "todo_authority");
    assert_eq!(MIGRATIONS[2].sql, TODO_MIGRATION);
    assert!(TODO_MIGRATION.contains("CREATE TABLE todos"));
    assert!(TODO_MIGRATION.contains("project_id uuid REFERENCES projects(id)"));
    assert!(TODO_MIGRATION.contains("CHECK (updated_at >= created_at)"));
    assert!(!TODO_MIGRATION.contains("parent_id"));
    assert!(!TODO_MIGRATION.contains("reminder_id"));
    assert!(!TODO_MIGRATION.contains("recurrence_rule"));
    assert!(!TODO_MIGRATION.contains("IF NOT EXISTS"));
    assert!(!TODO_MIGRATION.contains("DROP "));

    assert_eq!(MIGRATIONS[3].version, 4);
    assert_eq!(MIGRATIONS[3].name, "todo_tag_authority");
    assert_eq!(MIGRATIONS[3].sql, TODO_TAG_MIGRATION);
    assert!(TODO_TAG_MIGRATION.contains("CREATE TABLE todo_tags"));
    assert!(TODO_TAG_MIGRATION.contains("PRIMARY KEY (todo_id, tag_id)"));
    assert!(TODO_TAG_MIGRATION.contains("ON DELETE CASCADE"));
    assert!(TODO_TAG_MIGRATION.contains("ON DELETE RESTRICT"));
    assert!(!TODO_TAG_MIGRATION.contains("DROP "));

    assert_eq!(MIGRATIONS[4].version, 5);
    assert_eq!(MIGRATIONS[4].name, "authority_revision");
    assert_eq!(MIGRATIONS[4].sql, AUTHORITY_REVISION_MIGRATION);
    assert!(AUTHORITY_REVISION_MIGRATION.contains("CREATE TABLE mg_todo_authority_state"));
    assert!(
        AUTHORITY_REVISION_MIGRATION.contains("CREATE FUNCTION mg_todo_bump_authority_revision")
    );
    assert!(AUTHORITY_REVISION_MIGRATION.contains("CREATE TRIGGER mg_todo_projects_revision"));
    assert!(AUTHORITY_REVISION_MIGRATION.contains("CREATE TRIGGER mg_todo_tags_revision"));
    assert!(AUTHORITY_REVISION_MIGRATION.contains("CREATE TRIGGER mg_todo_todos_revision"));
    assert!(!AUTHORITY_REVISION_MIGRATION.contains("DROP "));
}

#[test]
fn migration_versions_are_strictly_increasing() {
    for pair in MIGRATIONS.windows(2) {
        assert!(pair[0].version < pair[1].version);
    }
}
