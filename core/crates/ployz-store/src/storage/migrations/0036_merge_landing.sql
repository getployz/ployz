-- What a pull request's first merge did in this PR Environment, written once when
-- it lands, as JSON: "no_destination" when it merged into a branch no Environment
-- deploys, else {"into":{<Destination ID>:<outcome>}} with one or more entries, each
-- {"outcome":"landed"} or {"outcome":"saved_not_deployed"|"not_landed","reason":<why>}.
-- NULL means no receipt: no merge yet, a merge from before this, or one a Store
-- without this column landed.
ALTER TABLE config_pr_environment ADD COLUMN merge_landing TEXT;
