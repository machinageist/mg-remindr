-- Establish one monotonic checkpoint for every persisted mg-todo authority change.
CREATE TABLE mg_todo_authority_state (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    revision bigint NOT NULL CHECK (revision >= 1),
    changed_at timestamptz NOT NULL
);

-- Serialize the initial checkpoint with concurrent authoritative writes.
LOCK TABLE projects, tags, todos IN SHARE ROW EXCLUSIVE MODE;

INSERT INTO mg_todo_authority_state (singleton, revision, changed_at)
SELECT true, 1, COALESCE(MAX(updated_at), TIMESTAMPTZ '1970-01-01 00:00:00+00')
FROM (
    SELECT updated_at FROM projects
    UNION ALL
    SELECT updated_at FROM tags
    UNION ALL
    SELECT updated_at FROM todos
) AS authority_rows;

CREATE FUNCTION mg_todo_bump_authority_revision()
RETURNS trigger
LANGUAGE plpgsql
SET search_path FROM CURRENT
AS $$
BEGIN
    UPDATE mg_todo_authority_state
    SET revision = revision + 1,
        changed_at = statement_timestamp()
    WHERE singleton = true;
    RETURN NULL;
END;
$$;

CREATE TRIGGER mg_todo_projects_revision_insert_delete
AFTER INSERT OR DELETE ON projects
FOR EACH ROW EXECUTE FUNCTION mg_todo_bump_authority_revision();
CREATE TRIGGER mg_todo_projects_revision_update
AFTER UPDATE ON projects
FOR EACH ROW
WHEN (OLD IS DISTINCT FROM NEW)
EXECUTE FUNCTION mg_todo_bump_authority_revision();

CREATE TRIGGER mg_todo_tags_revision_insert_delete
AFTER INSERT OR DELETE ON tags
FOR EACH ROW EXECUTE FUNCTION mg_todo_bump_authority_revision();
CREATE TRIGGER mg_todo_tags_revision_update
AFTER UPDATE ON tags
FOR EACH ROW
WHEN (OLD IS DISTINCT FROM NEW)
EXECUTE FUNCTION mg_todo_bump_authority_revision();

CREATE TRIGGER mg_todo_todos_revision_insert_delete
AFTER INSERT OR DELETE ON todos
FOR EACH ROW EXECUTE FUNCTION mg_todo_bump_authority_revision();
CREATE TRIGGER mg_todo_todos_revision_update
AFTER UPDATE ON todos
FOR EACH ROW
WHEN (OLD IS DISTINCT FROM NEW)
EXECUTE FUNCTION mg_todo_bump_authority_revision();
