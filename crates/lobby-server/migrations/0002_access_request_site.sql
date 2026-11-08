-- The client site (IPv4 address or IPv6 /48) and the mailbox (`+tag` aliases are one) of an Ask
-- to Join, so one site or mailbox cannot hold every pending place. Empty for a request made
-- before these columns.
ALTER TABLE access_requests ADD COLUMN site TEXT NOT NULL DEFAULT '';
ALTER TABLE access_requests ADD COLUMN mailbox TEXT NOT NULL DEFAULT '';
