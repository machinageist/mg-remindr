-- First todo authority slice preserves core todo state and project membership.
-- Parent, tag, dependency, recurrence, and reminder persistence remain deferred.
CREATE TABLE todos (
    id uuid PRIMARY KEY,
    title text NOT NULL CHECK (btrim(title) <> ''),
    project_id uuid REFERENCES projects(id),
    lifecycle text NOT NULL CHECK (lifecycle IN ('open', 'completed', 'trashed')),
    version bigint NOT NULL CHECK (version >= 1),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK (updated_at >= created_at)
);
