-- Persist cancellation before touching Redis so an uncertain admission cannot restart.
ALTER TABLE balance_holds ADD COLUMN cancel_requested BOOLEAN NOT NULL DEFAULT FALSE;
