-- Tag authority is independent of todo/tag relationships, which remain deferred.
CREATE TABLE tags (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (btrim(name) <> ''),
    version bigint NOT NULL CHECK (version >= 1),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK (updated_at >= created_at)
);
