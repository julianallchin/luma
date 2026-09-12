-- The last write guard on a synced table comes off.
--
-- `agent_thread_routing_identity_is_immutable` refused any UPDATE that changed
-- a thread's routing. A download is an upsert over every column, so the guard
-- fired on every thread this device already had — and a checkpoint is one
-- transaction, so one such thread took the whole checkpoint down. Nothing
-- downloaded at all, every iteration, silently.
--
-- Immutability is a rule about what this app does, and the app still does not
-- re-route a thread. It is not a rule about what a download may write: the row
-- the server sends is the row, and the only place that can be argued with is
-- the server. `20260914000000_local_write_guards.sql` says the same thing at
-- length; this trigger was simply missed.

DROP TRIGGER IF EXISTS agent_thread_routing_identity_is_immutable;
