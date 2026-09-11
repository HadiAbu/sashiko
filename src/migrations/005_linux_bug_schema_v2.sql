-- Migration 005: Linux bug workflow schema v2.
--
-- Renames every table owned by the Linux bug workflow to carry a workflow
-- prefix, so that additional workflows can add their own tables without
-- colliding. Shared infrastructure tables (people, subsystems, mailing_lists,
-- messages, threads) intentionally keep their unprefixed names.
--
--   bugs             -> bugs
--   bug_enrichments  -> bug_enrichments
--   bugs_subsystems  -> bug_subsystems
--   review_bugs      -> bug_reviews
--   (new)               bug_vectors
--
-- This is a deliberate clean break: the previous bug tables are dropped rather
-- than migrated, because the workflow is pre-production and the column layout
-- changes substantially.
--
-- Beyond the rename this migration:
--   * splits the overloaded status column into lifecycle_status (triage, owned
--     by humans) and pipeline_state (execution, owned by the worker), so that
--     re-running analysis can no longer destroy triage state,
--   * adds an assignee, so a bug can be assigned to whoever is working on it,
--   * adds worker lease columns for atomic claiming with retry limits,
--   * projects hot query fields out of enrichment JSON into indexed columns,
--   * moves the dedup embedding off the core row into bug_vectors,
--   * adds CHECK constraints so invalid states are unrepresentable.

DROP TRIGGER IF EXISTS trg_bugs_audit_insert;
DROP TRIGGER IF EXISTS trg_bugs_audit_status;
DROP TRIGGER IF EXISTS trg_bugs_audit_title;
DROP TRIGGER IF EXISTS trg_bugs_audit_dup_of_id;
DROP TRIGGER IF EXISTS trg_bugs_subsystems_audit_insert;

-- Children first so the drops stay valid once foreign keys are enforced.
DROP TABLE IF EXISTS bugs_subsystems;
DROP TABLE IF EXISTS review_bugs;
DROP TABLE IF EXISTS bug_enrichments;
DROP TABLE IF EXISTS bugs;

CREATE TABLE IF NOT EXISTS bugs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bugid TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,

    -- Triage lifecycle. Written by humans, the API, and the dedup stage.
    lifecycle_status TEXT NOT NULL DEFAULT 'new'
        CHECK (lifecycle_status IN
            ('new', 'open', 'fixed', 'dismissed', 'duplicate', 'closed')),

    -- Analysis execution state. Written exclusively by the bug worker.
    pipeline_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (pipeline_state IN
            ('pending', 'running', 'succeeded', 'failed', 'abandoned')),

    reporter TEXT NOT NULL,
    reported_at INTEGER NOT NULL,

    -- Assignment is a bare email address on purpose: Sashiko stores no
    -- persistent user records, so there is no table to reference here.
    assignee TEXT,
    assigned_at INTEGER,

    -- Immutable discovery provenance.
    discovered_in_patchset_id INTEGER,
    discovered_in_patch_id INTEGER,
    discovered_in_commit TEXT,
    source_ref TEXT,

    duplicate_of_id INTEGER,

    -- Worker lease. A claim is only valid until lease_expires_at, after which
    -- any worker may reclaim the bug.
    locked_by TEXT,
    lease_expires_at INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,

    -- Fields projected out of enrichment payloads by trigger so that listing
    -- and sorting do not need a correlated json_extract subquery per row.
    severity_int INTEGER NOT NULL DEFAULT 0 CHECK (severity_int BETWEEN 0 AND 4),
    verified_on_sha TEXT,
    introducing_commit_sha TEXT,
    fixed_in_commit_sha TEXT,

    -- Attribution side channel read by the audit triggers below. Writers set
    -- these in the same statement as the change being audited. They are NOT
    -- NULL with a default so that a writer which forgets degrades to 'system'
    -- instead of silently inheriting whoever touched the row previously.
    audit_author TEXT NOT NULL DEFAULT 'system',
    audit_tool TEXT NOT NULL DEFAULT 'system',
    audit_model TEXT,

    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,

    CHECK (duplicate_of_id IS NULL OR duplicate_of_id != id),
    -- A bug is marked duplicate if and only if it points at a canonical bug.
    CHECK ((lifecycle_status = 'duplicate') = (duplicate_of_id IS NOT NULL)),
    CHECK (assignee IS NULL OR length(trim(assignee)) > 0),
    CHECK (assignee IS NOT NULL OR assigned_at IS NULL),

    FOREIGN KEY(discovered_in_patchset_id) REFERENCES patchsets(id) ON DELETE SET NULL,
    FOREIGN KEY(discovered_in_patch_id) REFERENCES patches(id) ON DELETE SET NULL,
    -- RESTRICT rather than SET NULL: nulling duplicate_of_id would violate the
    -- lifecycle_status CHECK above, so duplicates must be re-parented first.
    FOREIGN KEY(duplicate_of_id) REFERENCES bugs(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_bugs_lifecycle ON bugs(lifecycle_status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_bugs_pipeline ON bugs(pipeline_state, created_at);
CREATE INDEX IF NOT EXISTS idx_bugs_assignee ON bugs(assignee, lifecycle_status);
CREATE INDEX IF NOT EXISTS idx_bugs_severity ON bugs(severity_int DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_bugs_reporter ON bugs(reporter);
CREATE INDEX IF NOT EXISTS idx_bugs_reported_at ON bugs(reported_at);
CREATE INDEX IF NOT EXISTS idx_bugs_duplicate_of_id ON bugs(duplicate_of_id);
CREATE INDEX IF NOT EXISTS idx_bugs_lease ON bugs(pipeline_state, lease_expires_at);

-- The enrichment kind vocabulary stays open on purpose: contributing a new kind
-- of analysis must not require a schema migration. Known kinds are modelled as
-- a Rust enum with an escape hatch for unrecognised values.
CREATE TABLE IF NOT EXISTS bug_enrichments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bug_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    tool TEXT NOT NULL,
    model TEXT,
    author TEXT,
    created_at INTEGER NOT NULL,
    content TEXT,
    data_json TEXT,
    tokens_in INTEGER,
    tokens_out INTEGER,
    tokens_cached INTEGER,
    logs TEXT,
    FOREIGN KEY(bug_id) REFERENCES bugs(id) ON DELETE CASCADE
);

-- Covers the (bug_id, created_at, id) ordering used when loading a bug's feed.
CREATE INDEX IF NOT EXISTS idx_bug_enrichments_bug_id ON bug_enrichments(bug_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_bug_enrichments_kind ON bug_enrichments(kind, bug_id);
CREATE INDEX IF NOT EXISTS idx_bug_enrichments_tool ON bug_enrichments(tool);

CREATE TABLE IF NOT EXISTS bug_subsystems (
    bug_id INTEGER NOT NULL,
    subsystem TEXT NOT NULL,
    PRIMARY KEY (bug_id, subsystem),
    FOREIGN KEY(bug_id) REFERENCES bugs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_bug_subsystems_subsystem ON bug_subsystems(subsystem, bug_id);
CREATE INDEX IF NOT EXISTS idx_bug_subsystems_bug_id ON bug_subsystems(bug_id);

CREATE TABLE IF NOT EXISTS bug_reviews (
    review_id INTEGER NOT NULL,
    bug_id INTEGER NOT NULL,
    is_newly_discovered INTEGER NOT NULL DEFAULT 1, -- 1 = newly discovered, 0 = matched existing
    PRIMARY KEY (review_id, bug_id),
    FOREIGN KEY(review_id) REFERENCES reviews(id) ON DELETE CASCADE,
    FOREIGN KEY(bug_id) REFERENCES bugs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_bug_reviews_review ON bug_reviews(review_id);
CREATE INDEX IF NOT EXISTS idx_bug_reviews_bug ON bug_reviews(bug_id);

-- Deduplication embeddings live outside the core row so that wide reads of a
-- bug do not drag the vector along. Keying on (bug_id, model) keeps older
-- embeddings intact when the embedding model changes.
CREATE TABLE IF NOT EXISTS bug_vectors (
    bug_id INTEGER NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    vector_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (bug_id, model),
    FOREIGN KEY(bug_id) REFERENCES bugs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_bug_vectors_bug_id ON bug_vectors(bug_id);

-- Audit triggers.
--
-- These record every mutation of an audited column as an 'audit' enrichment, so
-- the history survives even when a row is changed outside the application. The
-- null-safe IS NOT operator replaces the three-clause comparison used before.
--
-- pipeline_state is deliberately not audited: it churns on every analysis run
-- and would drown the human-readable feed.

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_insert
AFTER INSERT ON bugs
FOR EACH ROW
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        new.audit_tool,
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Bug created',
        json_object('action', 'created')
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_lifecycle_status
AFTER UPDATE OF lifecycle_status ON bugs
FOR EACH ROW
WHEN old.lifecycle_status IS NOT new.lifecycle_status
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        new.audit_tool,
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Status changed from "' || old.lifecycle_status || '" to "' || new.lifecycle_status || '"',
        json_object('field', 'lifecycle_status', 'old', old.lifecycle_status, 'new', new.lifecycle_status)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_title
AFTER UPDATE OF title ON bugs
FOR EACH ROW
WHEN old.title IS NOT new.title
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        new.audit_tool,
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Field "title" changed from "' || substr(IFNULL(old.title, 'null'), 1, 50) || '" to "' || substr(IFNULL(new.title, 'null'), 1, 50) || '"',
        json_object('field', 'title', 'old', old.title, 'new', new.title)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_duplicate_of_id
AFTER UPDATE OF duplicate_of_id ON bugs
FOR EACH ROW
WHEN old.duplicate_of_id IS NOT new.duplicate_of_id
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        new.audit_tool,
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        'Field "duplicate_of_id" changed from "' || IFNULL(CAST(old.duplicate_of_id AS TEXT), 'null') || '" to "' || IFNULL(CAST(new.duplicate_of_id AS TEXT), 'null') || '"',
        json_object('field', 'duplicate_of_id', 'old', old.duplicate_of_id, 'new', new.duplicate_of_id)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bugs_audit_assignee
AFTER UPDATE OF assignee ON bugs
FOR EACH ROW
WHEN old.assignee IS NOT new.assignee
BEGIN
    INSERT INTO bug_enrichments (
        bug_id, kind, tool, author, model, created_at, content, data_json
    ) VALUES (
        new.id,
        'audit',
        new.audit_tool,
        new.audit_author,
        new.audit_model,
        strftime('%s', 'now'),
        CASE
            WHEN new.assignee IS NULL THEN 'Unassigned from ' || old.assignee
            WHEN old.assignee IS NULL THEN 'Assigned to ' || new.assignee
            ELSE 'Reassigned from ' || old.assignee || ' to ' || new.assignee
        END,
        json_object('field', 'assignee', 'old', old.assignee, 'new', new.assignee)
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_bug_subsystems_audit_insert
AFTER INSERT ON bug_subsystems
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

-- Projection triggers.
--
-- The enrichment log stays the source of truth; these keep the denormalised
-- copies on bugs in step so that consistency does not depend on every
-- writer remembering to update them. Latest write wins, matching the
-- "ORDER BY created_at DESC LIMIT 1" semantics these columns replace.
--
-- None of these touch an audited column, so they cannot cascade back into the
-- audit triggers above.

CREATE TRIGGER IF NOT EXISTS trg_bug_enrichments_project_severity
AFTER INSERT ON bug_enrichments
FOR EACH ROW
WHEN new.kind = 'severity_calibration' AND new.data_json IS NOT NULL
BEGIN
    -- Precedence matches Bug::severity in db.rs: the severity string wins, and
    -- severity_int is only consulted when the string is absent or unknown.
    UPDATE bugs
    SET severity_int = COALESCE(
        CASE LOWER(json_extract(new.data_json, '$.severity'))
            WHEN 'critical' THEN 4
            WHEN 'high' THEN 3
            WHEN 'medium' THEN 2
            WHEN 'low' THEN 1
        END,
        CAST(json_extract(new.data_json, '$.severity_int') AS INTEGER),
        0)
    WHERE id = new.bug_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_bug_enrichments_project_verification
AFTER INSERT ON bug_enrichments
FOR EACH ROW
WHEN new.kind = 'verification'
    AND json_extract(new.data_json, '$.verified_on_sha') IS NOT NULL
BEGIN
    UPDATE bugs
    SET verified_on_sha = json_extract(new.data_json, '$.verified_on_sha')
    WHERE id = new.bug_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_bug_enrichments_project_origin
AFTER INSERT ON bug_enrichments
FOR EACH ROW
WHEN new.kind = 'origin_discovery'
    AND json_extract(new.data_json, '$.introducing_commit_sha') IS NOT NULL
BEGIN
    UPDATE bugs
    SET introducing_commit_sha = json_extract(new.data_json, '$.introducing_commit_sha')
    WHERE id = new.bug_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_bug_enrichments_project_fix
AFTER INSERT ON bug_enrichments
FOR EACH ROW
WHEN new.kind = 'fix_candidate'
    AND json_extract(new.data_json, '$.commit_sha') IS NOT NULL
BEGIN
    UPDATE bugs
    SET fixed_in_commit_sha = json_extract(new.data_json, '$.commit_sha')
    WHERE id = new.bug_id;
END;
