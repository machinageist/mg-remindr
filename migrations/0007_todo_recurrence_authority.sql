-- Recurrence is a bounded rule attached to one authoritative todo.
CREATE TABLE todo_recurrence (
    todo_id uuid PRIMARY KEY REFERENCES todos(id) ON DELETE CASCADE,
    start_date date NOT NULL,
    frequency text NOT NULL CHECK (frequency IN ('DAILY', 'WEEKLY', 'MONTHLY')),
    interval bigint NOT NULL CHECK (interval BETWEEN 1 AND 366),
    occurrence_count bigint CHECK (occurrence_count BETWEEN 1 AND 1000),
    until_date date,
    CHECK (occurrence_count IS NOT NULL OR until_date IS NOT NULL),
    CHECK (until_date IS NULL OR until_date > start_date)
);
