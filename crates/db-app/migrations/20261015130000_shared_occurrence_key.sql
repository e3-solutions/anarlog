-- Stable cross-account Google Calendar occurrence identity for workspace sharing.
ALTER TABLE events ADD COLUMN shared_occurrence_key TEXT NOT NULL DEFAULT '';
