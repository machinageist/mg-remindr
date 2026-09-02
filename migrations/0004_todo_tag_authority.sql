-- Todo/tag membership is an authoritative set owned by mg-todo.
-- Parent/dependency/recurrence/reminder relationships remain deferred.
CREATE TABLE todo_tags (
    todo_id uuid NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    tag_id uuid NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
    PRIMARY KEY (todo_id, tag_id)
);
