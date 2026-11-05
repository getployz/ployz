-- Arrivals hold only the states and columns their readers read: a state and `how` in
-- their sets, a pending row's `prior` and `was`, none of `prior`, `was` or an owner
-- once it is a Use hint or settled, and a Use hint a Follow's. The step that runs this
-- repaired first what it could without inventing a value.
--
-- NOT VALID: these check every write from here on, never a row stored before. A row
-- the repair couldn't fix stays as it was, and a read of it fails as before, rather
-- than every open of the Store. A later migration that rewrites every row of this
-- table, by an UPDATE of the whole table, meets the rows these never validated. No
-- rebuild either: one would drop the change-log triggers Cloud attaches.
ALTER TABLE config_sync_arrival
    ADD CONSTRAINT config_sync_arrival_state
        CHECK (state IN ('pending', 'hint', 'settled')) NOT VALID,
    ADD CONSTRAINT config_sync_arrival_how
        CHECK (how IN ('follow', 'sync')) NOT VALID,
    ADD CONSTRAINT config_sync_arrival_pending
        CHECK (state <> 'pending' OR (prior IS NOT NULL AND was IS NOT NULL)) NOT VALID,
    ADD CONSTRAINT config_sync_arrival_done
        CHECK (
            state = 'pending' OR (prior IS NULL AND was IS NULL AND proposal_id IS NULL)
        ) NOT VALID,
    ADD CONSTRAINT config_sync_arrival_hint
        CHECK (state <> 'hint' OR how = 'follow') NOT VALID;
