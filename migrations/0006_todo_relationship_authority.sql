-- Parent and dependency edges are authoritative mg-todo relationships.
CREATE TABLE todo_parents (
    child_id uuid PRIMARY KEY REFERENCES todos(id) ON DELETE CASCADE,
    parent_id uuid NOT NULL REFERENCES todos(id) ON DELETE RESTRICT,
    CHECK (child_id <> parent_id)
);

CREATE TABLE todo_dependencies (
    dependent_id uuid NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    prerequisite_id uuid NOT NULL REFERENCES todos(id) ON DELETE RESTRICT,
    PRIMARY KEY (dependent_id, prerequisite_id),
    CHECK (dependent_id <> prerequisite_id)
);
