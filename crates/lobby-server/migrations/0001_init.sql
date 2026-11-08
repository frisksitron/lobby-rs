-- Lobby schema.

CREATE TABLE lobby_settings (
  id            INTEGER PRIMARY KEY CHECK (id = 1),
  instance_id   TEXT    NOT NULL,                             -- lby_<24 hex>, LobbyInfo.instanceId
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  icon_blob_id  TEXT    REFERENCES blobs(id) ON DELETE SET NULL,
  owner_id      TEXT    REFERENCES users(id)                  -- NULL only before the first Member
) STRICT;

CREATE TABLE users (
  id             TEXT    PRIMARY KEY,                         -- usr_<24 hex>
  username       TEXT    NOT NULL COLLATE NOCASE UNIQUE,      -- 3..32 [A-Za-z0-9_-], checked in code
  email          TEXT    NOT NULL UNIQUE,                     -- lowercased; where this User's Magic Codes go
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER,
  deactivated_at INTEGER,                                     -- removed by the owner
  avatar_blob_id TEXT    REFERENCES blobs(id) ON DELETE SET NULL
) STRICT;
CREATE INDEX users_active ON users(deactivated_at) WHERE deactivated_at IS NULL;

CREATE TABLE sessions (
  id                 INTEGER PRIMARY KEY,
  user_id            TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  access_hash        BLOB    NOT NULL UNIQUE,
  access_expires_at  INTEGER NOT NULL,
  refresh_hash       BLOB    NOT NULL UNIQUE,
  refresh_expires_at INTEGER NOT NULL,                        -- sliding: now + 30 d at every rotation
  prev_refresh_hash  BLOB    UNIQUE,                          -- the token rotated away last (lost-response reissue)
  rotated_at         INTEGER,                                 -- when prev_refresh_hash was rotated away
  current_used_at    INTEGER,                                 -- first use of the current pair (NULL = never used)
  created_at         INTEGER NOT NULL,
  last_seen_at       INTEGER NOT NULL,                        -- updated at most once per rotation
  CHECK ((prev_refresh_hash IS NULL) = (rotated_at IS NULL))
) STRICT;
CREATE INDEX sessions_user ON sessions(user_id);
CREATE INDEX sessions_refresh_expiry ON sessions(refresh_expires_at);

CREATE TABLE messages (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,               -- = MessageId
  author_id  TEXT    NOT NULL REFERENCES users(id),
  content    TEXT    NOT NULL,                                -- sanitized HTML subset (<= 8000 chars raw, checked in code)
  nonce      TEXT    NOT NULL,                                -- the sender's MESSAGE_SEND rid
  created_at INTEGER NOT NULL,
  edited_at  INTEGER,                                         -- the author's last edit; NULL if never
  deleted_at INTEGER,                                         -- deleted rows stay, emptied, so a resend still matches
  rev        INTEGER,                                         -- the Lobby revision of the last edit or delete
  UNIQUE (author_id, nonce)                                   -- idempotent MESSAGE_SEND
) STRICT;
CREATE UNIQUE INDEX messages_rev ON messages(rev);

-- A Magic Code belongs to the request that asked for it; verify finds it by that challenge.
CREATE TABLE magic_codes (
  id             INTEGER PRIMARY KEY,
  challenge_hash BLOB    NOT NULL UNIQUE,                     -- sha256(challenge)
  email          TEXT    NOT NULL,
  code_hash      BLOB    NOT NULL,                            -- sha256(email ':' code)
  attempts       INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5),
  expires_at     INTEGER NOT NULL,
  used_at        INTEGER,
  created_at     INTEGER NOT NULL
) STRICT;
CREATE INDEX magic_codes_expiry ON magic_codes(expires_at);

-- A code checked for an email no Member uses: choosing a name spends it, once.
CREATE TABLE join_tickets (
  id         INTEGER PRIMARY KEY,
  email      TEXT    NOT NULL,
  token_hash BLOB    NOT NULL UNIQUE,                         -- sha256(ticket)
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX join_tickets_expiry ON join_tickets(expires_at);

CREATE TABLE access_requests (
  id                 TEXT    PRIMARY KEY,                     -- acr_<24 hex>
  username           TEXT    NOT NULL COLLATE NOCASE,
  email              TEXT    NOT NULL,                        -- requesterLabel on the wire
  status             TEXT    NOT NULL CHECK (status IN ('pending','vouched_in','declined')),
  handled_at         INTEGER,
  handled_by_user_id TEXT    REFERENCES users(id),
  approved_user_id   TEXT    REFERENCES users(id),
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL,
  CHECK (
    (status = 'pending'    AND handled_at IS NULL AND handled_by_user_id IS NULL AND approved_user_id IS NULL) OR
    (status = 'vouched_in' AND handled_at IS NOT NULL AND handled_by_user_id IS NOT NULL AND approved_user_id IS NOT NULL) OR
    (status = 'declined'   AND handled_at IS NOT NULL AND handled_by_user_id IS NOT NULL AND approved_user_id IS NULL)
  )
) STRICT;
CREATE UNIQUE INDEX access_requests_pending_email    ON access_requests(email)    WHERE status = 'pending';
CREATE UNIQUE INDEX access_requests_pending_username ON access_requests(username) WHERE status = 'pending';
CREATE INDEX access_requests_status_created ON access_requests(status, created_at DESC);

-- Waiting for a Vouch each device that asked (or checked with a code) holds a wait
-- token; once Vouched in, its first status check turns it into a session (used_at set).
CREATE TABLE join_waits (
  id         INTEGER PRIMARY KEY,
  request_id TEXT    NOT NULL REFERENCES access_requests(id) ON DELETE CASCADE,
  token_hash BLOB    NOT NULL UNIQUE,                         -- sha256(wait token)
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX join_waits_request ON join_waits(request_id);
CREATE INDEX join_waits_expiry ON join_waits(expires_at);

CREATE TABLE blobs (
  id             TEXT    PRIMARY KEY,                         -- blb_<24 hex>
  kind           TEXT    NOT NULL CHECK (kind IN ('avatar','lobby_icon','chat_attachment')),
  storage_path   TEXT    NOT NULL UNIQUE,                     -- relative, forward slashes
  mime_type      TEXT    NOT NULL,
  size_bytes     INTEGER NOT NULL CHECK (size_bytes > 0),
  original_name  TEXT    NOT NULL,                            -- basename, <= 255 chars
  uploaded_by    TEXT    NOT NULL REFERENCES users(id),
  message_seq    INTEGER REFERENCES messages(seq),            -- set when claimed
  preview_path   TEXT,
  preview_width  INTEGER,
  preview_height INTEGER,
  expires_at     INTEGER,                                     -- unclaimed chat attachments only
  created_at     INTEGER NOT NULL,
  CHECK ((preview_path IS NULL) = (preview_width IS NULL) AND (preview_path IS NULL) = (preview_height IS NULL)),
  CHECK (kind = 'chat_attachment' OR (message_seq IS NULL AND expires_at IS NULL))
) STRICT;
CREATE INDEX blobs_message ON blobs(message_seq);
CREATE INDEX blobs_unclaimed_expiry ON blobs(expires_at) WHERE message_seq IS NULL AND expires_at IS NOT NULL;

INSERT INTO lobby_settings (id, instance_id, created_at, updated_at)
VALUES (1, 'lby_' || lower(hex(randomblob(12))),
        CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));
