-- A due value is one consistent form: absent, an all-day civil date, or a zoned instant.
ALTER TABLE todos
    ADD COLUMN due_date date,
    ADD COLUMN due_at timestamptz,
    ADD COLUMN due_timezone text,
    ADD CONSTRAINT todos_due_is_one_consistent_form
        CHECK ((due_date IS NULL AND due_at IS NULL AND due_timezone IS NULL)
               OR (due_timezone IS NOT NULL AND ((due_date IS NULL) <> (due_at IS NULL)))),
    ADD CONSTRAINT todos_due_timezone_is_named
        CHECK (due_timezone IS NULL OR btrim(due_timezone) <> '');
