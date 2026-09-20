-- Author: Jeff
-- Date: 2026-09-19
-- Description: The whole mg-remindr authority in one SQLite schema
-- Notes: Identifiers are hyphenated UUID text, instants are fixed-width RFC 3339
--        microseconds in UTC, and civil dates are YYYY-MM-DD, so ordinary text
--        comparison is the same as time comparison. Append the next migration as
--        its own file; never edit this one once it has reached a store.

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (trim(name) <> ''),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('open', 'completed', 'trashed')),
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (updated_at >= created_at)
);

CREATE TABLE tags (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (trim(name) <> ''),
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (updated_at >= created_at)
);

-- A due value is one consistent form: absent, an all-day civil date, or a zoned instant.
-- A transition time is recorded when the transition is stored, never inferred from updated_at.
CREATE TABLE todos (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL CHECK (trim(title) <> ''),
    project_id TEXT REFERENCES projects(id),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('open', 'completed', 'trashed')),
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    trashed_at TEXT,
    due_date TEXT,
    due_at TEXT,
    due_timezone TEXT,
    CHECK (updated_at >= created_at),
    CHECK ((lifecycle = 'completed') = (completed_at IS NOT NULL)),
    CHECK ((lifecycle = 'trashed') = (trashed_at IS NOT NULL)),
    CHECK (completed_at IS NULL OR completed_at BETWEEN created_at AND updated_at),
    CHECK (trashed_at IS NULL OR trashed_at BETWEEN created_at AND updated_at),
    CHECK ((due_date IS NULL AND due_at IS NULL AND due_timezone IS NULL)
           OR (due_timezone IS NOT NULL AND ((due_date IS NULL) <> (due_at IS NULL)))),
    CHECK (due_timezone IS NULL OR trim(due_timezone) <> '')
);

-- Todo/tag membership is an authoritative set owned by mg-remindr
CREATE TABLE todo_tags (
    todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
    PRIMARY KEY (todo_id, tag_id)
);

-- Parent and dependency edges are authoritative mg-remindr relationships
CREATE TABLE todo_parents (
    child_id TEXT PRIMARY KEY REFERENCES todos(id) ON DELETE CASCADE,
    parent_id TEXT NOT NULL REFERENCES todos(id) ON DELETE RESTRICT,
    CHECK (child_id <> parent_id)
);

CREATE TABLE todo_dependencies (
    dependent_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    prerequisite_id TEXT NOT NULL REFERENCES todos(id) ON DELETE RESTRICT,
    PRIMARY KEY (dependent_id, prerequisite_id),
    CHECK (dependent_id <> prerequisite_id)
);

-- Recurrence is a bounded rule attached to one authoritative todo
CREATE TABLE todo_recurrence (
    todo_id TEXT PRIMARY KEY REFERENCES todos(id) ON DELETE CASCADE,
    start_date TEXT NOT NULL,
    frequency TEXT NOT NULL CHECK (frequency IN ('DAILY', 'WEEKLY', 'MONTHLY')),
    interval INTEGER NOT NULL CHECK (interval BETWEEN 1 AND 366),
    occurrence_count INTEGER CHECK (occurrence_count BETWEEN 1 AND 1000),
    until_date TEXT,
    CHECK (occurrence_count IS NOT NULL OR until_date IS NOT NULL),
    CHECK (until_date IS NULL OR until_date > start_date)
);

-- Reminder schedules are authoritative mg-remindr records
CREATE TABLE todo_reminders (
    id TEXT PRIMARY KEY,
    todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    remind_at TEXT NOT NULL,
    channel TEXT NOT NULL CHECK (channel IN ('TUI', 'DESKTOP', 'WEBHOOK')),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'paused', 'cancelled')),
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (updated_at >= created_at)
);

-- Delivery attempts are durable, idempotent mg-remindr records
CREATE TABLE todo_reminder_deliveries (
    id TEXT PRIMARY KEY,
    reminder_id TEXT NOT NULL REFERENCES todo_reminders(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed')),
    attempted_at TEXT,
    provider_reference TEXT,
    failure_code TEXT,
    created_at TEXT NOT NULL,
    CHECK (trim(idempotency_key) <> ''),
    CHECK (attempted_at IS NULL OR attempted_at >= created_at),
    CHECK (status <> 'sent' OR provider_reference IS NOT NULL),
    CHECK (status <> 'failed' OR failure_code IS NOT NULL),
    CHECK (status = 'pending' OR attempted_at IS NOT NULL),
    CHECK (status <> 'pending' OR attempted_at IS NULL)
);

-- One monotonic checkpoint for every persisted mg-remindr authority change.
-- An export carries it, so mg-calr can tell one snapshot from the next.
CREATE TABLE mg_remindr_authority_state (
    singleton INTEGER PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    changed_at TEXT NOT NULL
);

INSERT INTO mg_remindr_authority_state (singleton, revision, changed_at)
VALUES (1, 1, '1970-01-01T00:00:00.000000Z');

-- The checkpoint belongs to the database, not to the caller: every authoritative
-- write bumps it whether it came from the CLI, the import path, or sqlite3 itself.
-- An update that changes nothing is not a change, which is why each one is guarded;
-- IS NOT is null-safe, the way PostgreSQL's IS DISTINCT FROM was.
CREATE TRIGGER mg_remindr_projects_revision_insert AFTER INSERT ON projects
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_projects_revision_delete AFTER DELETE ON projects
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_projects_revision_update AFTER UPDATE ON projects
WHEN OLD.name IS NOT NEW.name
    OR OLD.lifecycle IS NOT NEW.lifecycle
    OR OLD.version IS NOT NEW.version
    OR OLD.created_at IS NOT NEW.created_at
    OR OLD.updated_at IS NOT NEW.updated_at
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_tags_revision_insert AFTER INSERT ON tags
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_tags_revision_delete AFTER DELETE ON tags
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_tags_revision_update AFTER UPDATE ON tags
WHEN OLD.name IS NOT NEW.name
    OR OLD.version IS NOT NEW.version
    OR OLD.created_at IS NOT NEW.created_at
    OR OLD.updated_at IS NOT NEW.updated_at
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_todos_revision_insert AFTER INSERT ON todos
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_todos_revision_delete AFTER DELETE ON todos
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;

CREATE TRIGGER mg_remindr_todos_revision_update AFTER UPDATE ON todos
WHEN OLD.title IS NOT NEW.title
    OR OLD.project_id IS NOT NEW.project_id
    OR OLD.lifecycle IS NOT NEW.lifecycle
    OR OLD.version IS NOT NEW.version
    OR OLD.created_at IS NOT NEW.created_at
    OR OLD.updated_at IS NOT NEW.updated_at
    OR OLD.completed_at IS NOT NEW.completed_at
    OR OLD.trashed_at IS NOT NEW.trashed_at
    OR OLD.due_date IS NOT NEW.due_date
    OR OLD.due_at IS NOT NEW.due_at
    OR OLD.due_timezone IS NOT NEW.due_timezone
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1, changed_at = strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')
    WHERE singleton = 1;
END;
