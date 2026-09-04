-- Transition times are recorded when the transition is stored, never inferred from updated_at.
ALTER TABLE todos
    ADD COLUMN completed_at timestamptz,
    ADD COLUMN trashed_at timestamptz,
    ADD CONSTRAINT todos_completed_at_matches_lifecycle
        CHECK ((lifecycle = 'completed') = (completed_at IS NOT NULL)),
    ADD CONSTRAINT todos_trashed_at_matches_lifecycle
        CHECK ((lifecycle = 'trashed') = (trashed_at IS NOT NULL)),
    ADD CONSTRAINT todos_completed_at_within_history
        CHECK (completed_at IS NULL OR completed_at BETWEEN created_at AND updated_at),
    ADD CONSTRAINT todos_trashed_at_within_history
        CHECK (trashed_at IS NULL OR trashed_at BETWEEN created_at AND updated_at);
