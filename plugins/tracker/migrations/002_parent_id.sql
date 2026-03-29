ALTER TABLE issues ADD COLUMN parent_id TEXT REFERENCES issues(id);
