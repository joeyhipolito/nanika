CREATE TABLE runtime_schema (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version > 0)
                 );
                 INSERT INTO runtime_schema(singleton, version) VALUES (1, 1);

                 CREATE TABLE journal (
                    sequence INTEGER PRIMARY KEY CHECK (sequence > 0),
                    transition_id TEXT NOT NULL UNIQUE,
                    mission_id TEXT,
                    record_schema_version INTEGER NOT NULL CHECK (record_schema_version > 0),
                    transition_kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    committed_at_utc TEXT NOT NULL,
                    extra_json TEXT NOT NULL,
                    outbox_fingerprint TEXT NOT NULL,
                    projection_fingerprint TEXT NOT NULL,
                    previous_checksum TEXT NOT NULL,
                    checksum TEXT NOT NULL UNIQUE
                 );
                 CREATE TRIGGER journal_reject_update
                    BEFORE UPDATE ON journal
                    BEGIN SELECT RAISE(ABORT, 'journal records are immutable'); END;
                 CREATE TRIGGER journal_reject_delete
                    BEFORE DELETE ON journal
                    BEGIN SELECT RAISE(ABORT, 'journal records are immutable'); END;

                 CREATE TABLE outbox (
                    idempotency_key TEXT PRIMARY KEY,
                    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
                    mission_id TEXT NOT NULL,
                    phase_id TEXT,
                    effect_kind TEXT NOT NULL CHECK (effect_kind IN ('provider_process','git_command','plugin_process')),
                    logical_attempt INTEGER NOT NULL CHECK (logical_attempt > 0),
                    payload_json TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('pending','executing','succeeded','failed','uncertain')),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
                    updated_at_utc TEXT NOT NULL
                 );
                 CREATE INDEX outbox_delivery_order
                    ON outbox(state, journal_sequence, idempotency_key);
                 CREATE TRIGGER outbox_reject_immutable_update
                    BEFORE UPDATE OF idempotency_key, journal_sequence, mission_id, phase_id,
                                     effect_kind, logical_attempt, payload_json
                    ON outbox
                    BEGIN SELECT RAISE(ABORT, 'outbox identity and intent are immutable'); END;
                 CREATE TRIGGER outbox_reject_delete
                    BEFORE DELETE ON outbox
                    BEGIN SELECT RAISE(ABORT, 'outbox records are retained'); END;

                 CREATE TABLE outbox_attempt_claim (
                    idempotency_key TEXT NOT NULL REFERENCES outbox(idempotency_key) ON DELETE RESTRICT,
                    attempt INTEGER NOT NULL CHECK (attempt > 0),
                    claimed_at_utc TEXT NOT NULL,
                    PRIMARY KEY (idempotency_key, attempt)
                 );
                 CREATE TRIGGER outbox_attempt_claim_reject_update
                    BEFORE UPDATE ON outbox_attempt_claim
                    BEGIN SELECT RAISE(ABORT, 'outbox attempt claims are immutable'); END;
                 CREATE TRIGGER outbox_attempt_claim_reject_delete
                    BEFORE DELETE ON outbox_attempt_claim
                    BEGIN SELECT RAISE(ABORT, 'outbox attempt claims are retained'); END;

                 CREATE TABLE outbox_execution_identity (
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    pid INTEGER NOT NULL CHECK (pid > 0),
                    process_group_id INTEGER NOT NULL CHECK (process_group_id > 0),
                    process_start_identity TEXT NOT NULL,
                    recorded_at_utc TEXT NOT NULL,
                    PRIMARY KEY (idempotency_key, attempt),
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE TRIGGER outbox_execution_identity_reject_update
                    BEFORE UPDATE ON outbox_execution_identity
                    BEGIN SELECT RAISE(ABORT, 'process execution identities are immutable'); END;
                 CREATE TRIGGER outbox_execution_identity_reject_delete
                    BEFORE DELETE ON outbox_execution_identity
                    BEGIN SELECT RAISE(ABORT, 'process execution identities are retained'); END;

                 CREATE TABLE outbox_attempt_observation (
                    observation_sequence INTEGER PRIMARY KEY CHECK (observation_sequence > 0),
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    observed_state TEXT NOT NULL CHECK (observed_state IN ('pending','succeeded','failed','uncertain')),
                    evidence_code TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    observed_at_utc TEXT NOT NULL,
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE INDEX outbox_observation_history
                    ON outbox_attempt_observation(idempotency_key, attempt, observation_sequence);
                 CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;
                 CREATE TRIGGER outbox_attempt_observation_reject_delete
                    BEFORE DELETE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are retained'); END;

                 CREATE TABLE projection_requirement (
                    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
                    projection TEXT NOT NULL CHECK (projection IN ('checkpoint','event_log','workspace','sidecars','metrics')),
                    PRIMARY KEY (journal_sequence, projection)
                 );
                 CREATE TRIGGER projection_requirement_reject_update
                    BEFORE UPDATE ON projection_requirement
                    BEGIN SELECT RAISE(ABORT, 'projection requirements are immutable'); END;
                 CREATE TRIGGER projection_requirement_reject_delete
                    BEFORE DELETE ON projection_requirement
                    BEGIN SELECT RAISE(ABORT, 'projection requirements are retained'); END;

                 CREATE TABLE projection_receipt (
                    journal_sequence INTEGER NOT NULL,
                    projection TEXT NOT NULL,
                    public_sequence INTEGER CHECK (public_sequence IS NULL OR public_sequence > 0),
                    event_id TEXT,
                    applied_at_utc TEXT NOT NULL,
                    PRIMARY KEY (journal_sequence, projection),
                    FOREIGN KEY (journal_sequence, projection)
                      REFERENCES projection_requirement(journal_sequence, projection)
                      ON DELETE RESTRICT,
                    CHECK (
                      (projection = 'event_log' AND public_sequence IS NOT NULL AND event_id IS NOT NULL)
                      OR
                      (projection != 'event_log' AND public_sequence IS NULL AND event_id IS NULL)
                    )
                 );
                 CREATE TRIGGER projection_receipt_reject_update
                    BEFORE UPDATE ON projection_receipt
                    BEGIN SELECT RAISE(ABORT, 'projection receipts are immutable'); END;
                 CREATE TRIGGER projection_receipt_reject_delete
                    BEFORE DELETE ON projection_receipt
                    BEGIN SELECT RAISE(ABORT, 'projection receipts are retained'); END;

                 CREATE TABLE command_ack (
                    journal_sequence INTEGER PRIMARY KEY REFERENCES journal(sequence) ON DELETE RESTRICT,
                    acknowledged_at_utc TEXT NOT NULL
                 );
                 CREATE TRIGGER command_ack_reject_update
                    BEFORE UPDATE ON command_ack
                    BEGIN SELECT RAISE(ABORT, 'command acknowledgements are immutable'); END;
                 CREATE TRIGGER command_ack_reject_delete
                    BEFORE DELETE ON command_ack
                    BEGIN SELECT RAISE(ABORT, 'command acknowledgements are retained'); END;

                 CREATE TABLE projection_cursor (
                    projection TEXT PRIMARY KEY,
                    journal_sequence INTEGER NOT NULL CHECK (journal_sequence >= 0),
                    public_sequence INTEGER CHECK (public_sequence IS NULL OR public_sequence > 0),
                    event_id TEXT,
                    updated_at_utc TEXT NOT NULL
                 );
                 PRAGMA user_version = 1;
