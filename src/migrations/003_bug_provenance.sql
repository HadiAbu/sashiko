-- Migration 003: Add bug provenance columns and attribution audit triggers

ALTER TABLE bugs ADD COLUMN audit_author TEXT;
ALTER TABLE bugs ADD COLUMN audit_tool TEXT;
ALTER TABLE bugs ADD COLUMN audit_model TEXT;

DROP TRIGGER IF EXISTS trg_bugs_audit_status;
DROP TRIGGER IF EXISTS trg_bugs_audit_title;
DROP TRIGGER IF EXISTS trg_bugs_audit_dup_of_id;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_insert
AFTER INSERT ON bugs
FOR EACH ROW
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        IFNULL(new.audit_tool, 'system'),
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Bug created',
        json_object('action', 'created')
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_status
AFTER UPDATE OF status ON bugs
FOR EACH ROW
WHEN (old.status != new.status) OR (old.status IS NULL AND new.status IS NOT NULL) OR (old.status IS NOT NULL AND new.status IS NULL)
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        IFNULL(new.audit_tool, 'system'),
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Field "status" changed from "' || substr(IFNULL(CAST(old.status AS TEXT), 'null'), 1, 50) || '" to "' || substr(IFNULL(CAST(new.status AS TEXT), 'null'), 1, 50) || '"',
        json_object('field', 'status', 'old', old.status, 'new', new.status)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_title
AFTER UPDATE OF title ON bugs
FOR EACH ROW
WHEN (old.title != new.title) OR (old.title IS NULL AND new.title IS NOT NULL) OR (old.title IS NOT NULL AND new.title IS NULL)
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        IFNULL(new.audit_tool, 'system'),
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Field "title" changed from "' || substr(IFNULL(CAST(old.title AS TEXT), 'null'), 1, 50) || '" to "' || substr(IFNULL(CAST(new.title AS TEXT), 'null'), 1, 50) || '"',
        json_object('field', 'title', 'old', old.title, 'new', new.title)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_dup_of_id
AFTER UPDATE OF duplicate_of_id ON bugs
FOR EACH ROW
WHEN (old.duplicate_of_id != new.duplicate_of_id) OR (old.duplicate_of_id IS NULL AND new.duplicate_of_id IS NOT NULL) OR (old.duplicate_of_id IS NOT NULL AND new.duplicate_of_id IS NULL)
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        IFNULL(new.audit_tool, 'system'),
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Field "duplicate_of_id" changed from "' || substr(IFNULL(CAST(old.duplicate_of_id AS TEXT), 'null'), 1, 50) || '" to "' || substr(IFNULL(CAST(new.duplicate_of_id AS TEXT), 'null'), 1, 50) || '"',
        json_object('field', 'duplicate_of_id', 'old', old.duplicate_of_id, 'new', new.duplicate_of_id)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_subsystems_audit_insert
AFTER INSERT ON bugs_subsystems
FOR EACH ROW
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.bug_id,
        'audit',
        IFNULL((SELECT audit_tool FROM bugs WHERE id = new.bug_id), 'system'),
        (SELECT audit_author FROM bugs WHERE id = new.bug_id),
        (SELECT audit_model FROM bugs WHERE id = new.bug_id),
        strftime('%s', 'now'),
        'Subsystem "' || new.subsystem || '" added',
        json_object('action', 'subsystem_added', 'subsystem', new.subsystem)
    );
END;
