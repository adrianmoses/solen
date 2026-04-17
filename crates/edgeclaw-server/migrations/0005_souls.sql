CREATE TABLE IF NOT EXISTS souls (
    user_id         TEXT PRIMARY KEY REFERENCES users(id),
    name            TEXT NOT NULL DEFAULT 'Assistant',
    personality     TEXT NOT NULL DEFAULT '',
    archetype       TEXT NOT NULL DEFAULT 'assistant',
    tone            TEXT NOT NULL DEFAULT 'neutral',
    verbosity       TEXT NOT NULL DEFAULT 'balanced',
    decision_style  TEXT NOT NULL DEFAULT 'balanced',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
