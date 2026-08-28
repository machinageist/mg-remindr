use mg_todo::storage::{FOUNDATION_MIGRATION, MIGRATIONS};

#[test]
fn foundation_migration_is_embedded_and_append_only() {
    assert_eq!(MIGRATIONS.len(), 1);
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[0].name, "project_authority");
    assert_eq!(MIGRATIONS[0].sql, FOUNDATION_MIGRATION);
    assert!(FOUNDATION_MIGRATION.contains("CREATE TABLE projects"));
    assert!(FOUNDATION_MIGRATION.contains("version bigint NOT NULL"));
    assert!(FOUNDATION_MIGRATION.contains("CHECK (version >= 1)"));
    assert!(FOUNDATION_MIGRATION.contains("CHECK (lifecycle IN ('open', 'completed', 'trashed'))"));
    assert!(!FOUNDATION_MIGRATION.contains("DROP TABLE"));
    assert!(!FOUNDATION_MIGRATION.contains("CREATE EXTENSION"));
}

#[test]
fn migration_versions_are_strictly_increasing() {
    for pair in MIGRATIONS.windows(2) {
        assert!(pair[0].version < pair[1].version);
    }
}
