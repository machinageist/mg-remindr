-- Reminder schedules are authoritative mg-todo records.
CREATE TABLE todo_reminders (
    id uuid PRIMARY KEY,
    todo_id uuid NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    remind_at timestamptz NOT NULL,
    channel text NOT NULL CHECK (channel IN ('TUI', 'DESKTOP', 'WEBHOOK')),
    lifecycle text NOT NULL CHECK (lifecycle IN ('active', 'paused', 'cancelled')),
    version bigint NOT NULL CHECK (version >= 1),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK (updated_at >= created_at)
);
