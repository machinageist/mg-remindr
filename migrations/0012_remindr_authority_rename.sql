-- Carry the todo-era authority objects over to the mg-remindr application name.
-- Renames only: no column, constraint, or row is altered by this migration.
ALTER TABLE mg_todo_authority_state RENAME TO mg_remindr_authority_state;

ALTER FUNCTION mg_todo_bump_authority_revision() RENAME TO mg_remindr_bump_authority_revision;

-- The trigger function body names the checkpoint table literally and is resolved
-- at call time, so the rename above leaves it pointing at a table that no longer
-- exists. Replace the body in the same transaction. Existing triggers bind the
-- function by OID and keep firing across both statements.
CREATE OR REPLACE FUNCTION mg_remindr_bump_authority_revision()
RETURNS trigger
LANGUAGE plpgsql
SET search_path FROM CURRENT
AS $$
BEGIN
    UPDATE mg_remindr_authority_state
    SET revision = revision + 1,
        changed_at = statement_timestamp()
    WHERE singleton = true;
    RETURN NULL;
END;
$$;

ALTER TRIGGER mg_todo_projects_revision_insert_delete ON projects
RENAME TO mg_remindr_projects_revision_insert_delete;
ALTER TRIGGER mg_todo_projects_revision_update ON projects
RENAME TO mg_remindr_projects_revision_update;

ALTER TRIGGER mg_todo_tags_revision_insert_delete ON tags
RENAME TO mg_remindr_tags_revision_insert_delete;
ALTER TRIGGER mg_todo_tags_revision_update ON tags
RENAME TO mg_remindr_tags_revision_update;

ALTER TRIGGER mg_todo_todos_revision_insert_delete ON todos
RENAME TO mg_remindr_todos_revision_insert_delete;
ALTER TRIGGER mg_todo_todos_revision_update ON todos
RENAME TO mg_remindr_todos_revision_update;
