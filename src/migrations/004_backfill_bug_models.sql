-- Migration 004: Backfill model for existing bugs and candidate enrichments

UPDATE bug_enrichments
SET model = (
    SELECT r.model
    FROM review_bugs rb
    JOIN reviews r ON r.id = rb.review_id
    WHERE rb.bug_id = bug_enrichments.bug_id AND r.model IS NOT NULL
    LIMIT 1
)
WHERE (model IS NULL OR trim(model) = '')
  AND kind IN ('candidate', 'discovery')
  AND EXISTS (
      SELECT 1 FROM review_bugs rb
      JOIN reviews r ON r.id = rb.review_id
      WHERE rb.bug_id = bug_enrichments.bug_id AND r.model IS NOT NULL
  );

UPDATE bug_enrichments
SET model = (
    SELECT r.model
    FROM bugs b
    JOIN review_bugs rb ON rb.bug_id = b.duplicate_of_id
    JOIN reviews r ON r.id = rb.review_id
    WHERE b.id = bug_enrichments.bug_id AND r.model IS NOT NULL
    LIMIT 1
)
WHERE (model IS NULL OR trim(model) = '')
  AND kind IN ('candidate', 'discovery')
  AND EXISTS (
      SELECT 1 FROM bugs b
      JOIN review_bugs rb ON rb.bug_id = b.duplicate_of_id
      JOIN reviews r ON r.id = rb.review_id
      WHERE b.id = bug_enrichments.bug_id AND r.model IS NOT NULL
  );

UPDATE bug_enrichments
SET model = (
    SELECT r.model
    FROM bugs b
    JOIN reviews r ON r.patchset_id = b.discovered_in_patchset_id
    WHERE b.id = bug_enrichments.bug_id AND r.model IS NOT NULL
    LIMIT 1
)
WHERE (model IS NULL OR trim(model) = '')
  AND kind IN ('candidate', 'discovery')
  AND EXISTS (
      SELECT 1 FROM bugs b
      JOIN reviews r ON r.patchset_id = b.discovered_in_patchset_id
      WHERE b.id = bug_enrichments.bug_id AND r.model IS NOT NULL
  );
