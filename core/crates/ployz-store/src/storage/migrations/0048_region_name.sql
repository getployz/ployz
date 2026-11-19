-- The name each region shows as, by its key: the `region` label value its Servers carry.
-- Display only: placement reads the label, never this. A region with no row shows its
-- built-in name.
CREATE TABLE config_region_name (
    organization_id TEXT NOT NULL,
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (organization_id, key)
);
