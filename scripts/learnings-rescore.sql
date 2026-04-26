-- learnings-rescore.sql
-- One-shot proxy rescore for ~/.alluka/learnings.db quality_score freeze (2026-02-20 → 2026-04-24)
-- Root cause: commit 1643073e deleted DB.UpdateQualityScores(); 6,076 of 6,260 active rows stuck at 0.0 or 0.4.
-- Formula: base_tier(type) × recency_decay(age_days) × (1 + capped injection_boost) × compliance_multiplier
-- Max attainable ≈ 1.5; min ≈ 0.04. Absolute magnitude does not matter — preflight uses ORDER BY, only rank does.
-- Backup before running: cp ~/.alluka/learnings.db ~/.alluka/learnings.db.bak.$(date -u +%Y%m%dT%H%M%SZ)
-- Reversible: restore from backup. Idempotent: re-running produces identical result.

BEGIN TRANSACTION;

UPDATE learnings SET quality_score =
  -- Base tier by type (from archived pre-v2 scoring formula)
  (CASE type
    WHEN 'insight'    THEN 1.0
    WHEN 'decision'   THEN 0.8
    WHEN 'pattern'    THEN 0.7
    WHEN 'error'      THEN 0.6
    WHEN 'source'     THEN 0.4
    WHEN 'preference' THEN 0.3
    WHEN 'behavior'   THEN 0.3
    ELSE 0.5
  END)
  *
  -- Recency decay (step-function; SQL-portable across sqlite versions)
  (CASE
    WHEN julianday('now') - julianday(created_at) <= 7   THEN 1.0
    WHEN julianday('now') - julianday(created_at) <= 30  THEN 0.9
    WHEN julianday('now') - julianday(created_at) <= 90  THEN 0.75
    WHEN julianday('now') - julianday(created_at) <= 180 THEN 0.6
    WHEN julianday('now') - julianday(created_at) <= 365 THEN 0.4
    ELSE 0.25
  END)
  *
  -- Injection boost (log-capped — prevents legacy cohort from reinforcing incumbency)
  (1 + CASE
    WHEN injection_count = 0  THEN 0
    WHEN injection_count <= 2 THEN 0.2
    WHEN injection_count <= 5 THEN 0.3
    WHEN injection_count <= 10 THEN 0.4
    ELSE 0.5
  END)
  *
  -- Compliance multiplier (neutral 0.75 when no compliance data yet)
  (CASE
    WHEN compliance_count = 0 THEN 0.75
    ELSE (0.5 + 0.5 * compliance_rate)
  END)
WHERE archived = 0;

COMMIT;
