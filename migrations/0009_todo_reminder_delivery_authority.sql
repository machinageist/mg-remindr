-- Delivery attempts are durable, idempotent mg-todo records.
CREATE TABLE todo_reminder_deliveries (
    id uuid PRIMARY KEY,
    reminder_id uuid NOT NULL REFERENCES todo_reminders(id) ON DELETE CASCADE,
    idempotency_key text NOT NULL UNIQUE,
    status text NOT NULL CHECK (status IN ('pending', 'sent', 'failed')),
    attempted_at timestamptz,
    provider_reference text,
    failure_code text,
    created_at timestamptz NOT NULL,
    CHECK (btrim(idempotency_key) <> ''),
    CHECK (attempted_at IS NULL OR attempted_at >= created_at),
    CHECK (status <> 'sent' OR provider_reference IS NOT NULL),
    CHECK (status <> 'failed' OR failure_code IS NOT NULL),
    CHECK (status = 'pending' OR attempted_at IS NOT NULL),
    CHECK (status <> 'pending' OR attempted_at IS NULL)
);
