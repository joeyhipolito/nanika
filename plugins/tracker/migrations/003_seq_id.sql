-- Add seq_id column to issues table
ALTER TABLE issues ADD COLUMN seq_id INTEGER;

-- Populate seq_id for existing issues ordered by created_at
-- Use ROW_NUMBER to assign sequential IDs starting from 1
WITH ranked AS (
  SELECT id, ROW_NUMBER() OVER (ORDER BY created_at ASC) as rn
  FROM issues
)
UPDATE issues SET seq_id = (
  SELECT rn FROM ranked WHERE ranked.id = issues.id
);

-- Create unique index on seq_id (allow NULL for any future edge cases)
CREATE UNIQUE INDEX idx_issues_seq_id ON issues(seq_id) WHERE seq_id IS NOT NULL;
