ALTER TABLE credentials ADD COLUMN credential_type TEXT NOT NULL DEFAULT 'oauth';
ALTER TABLE credentials ADD COLUMN metadata_enc BLOB;
