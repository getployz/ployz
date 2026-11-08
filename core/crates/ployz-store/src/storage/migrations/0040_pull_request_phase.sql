-- Each pull request's phase, one tagged JSON value: open or closed with its head and
-- standing, or merged for good with where it went and what it didn't land. A Store
-- from before this one still reads and writes `facts`, `merged`, `unlanded` and
-- `standing`, which this one keeps writing; a row it writes since reads from those.
ALTER TABLE config_pull_request ADD COLUMN phase TEXT;
