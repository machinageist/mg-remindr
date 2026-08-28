-- The first authority-bearing persistence slice is deliberately limited to projects.
CREATE TABLE projects (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (btrim(name) <> ''),
    lifecycle text NOT NULL CHECK (lifecycle IN ('open', 'completed', 'trashed')),
    version bigint NOT NULL CHECK (version >= 1),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK (updated_at >= created_at)
);
