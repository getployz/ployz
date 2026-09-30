-- The Config Store's schema. Every table carries organization_id for Cloud's change
-- log. What an Environment owns goes with it (ON DELETE CASCADE), and an
-- Environment goes with its Project; a Branch's Parent can't go before it.

-- Projects and their Environments, each with one Working State document.
CREATE TABLE config_project (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    name TEXT NOT NULL,
    default_environment_id TEXT NOT NULL,
    UNIQUE (organization_id, name)
);

CREATE TABLE config_environment (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES config_project (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    working_revision BIGINT NOT NULL,
    working TEXT NOT NULL,
    -- Setup Commands a new Branch of it runs when it names none (JSON; NULL: none).
    branch_setup TEXT,
    UNIQUE (project_id, name)
);

-- Each node as first created in its Environment; never changes after.
CREATE TABLE config_node_introduction (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    node_type TEXT NOT NULL,
    node TEXT NOT NULL,
    PRIMARY KEY (environment_id, node_id)
);

-- The caller-minted IDs of every create, with what it wrote, for replay.
CREATE TABLE config_create (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    command TEXT NOT NULL,
    written TEXT NOT NULL
);

-- Saved State: one immutable document per Saved revision, numbered from 1.
CREATE TABLE config_saved (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    organization_id TEXT NOT NULL,
    intent TEXT NOT NULL,
    PRIMARY KEY (environment_id, revision)
);

-- The runtime Namespace each Environment deploys into, fixed at its first
-- admission and unique in its Organization.
CREATE TABLE config_namespace (
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    environment_id TEXT NOT NULL UNIQUE REFERENCES config_environment (id) ON DELETE CASCADE,
    PRIMARY KEY (organization_id, namespace)
);

-- Deployments: each admitted attempt, frozen at admission (Saved revision, target
-- nodes, Namespace, sealed registry credentials, upload, Cluster Domain), who
-- admitted it and when, and its runner's claim, lease and evidence (`run`).
CREATE TABLE config_deployment (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    number BIGINT NOT NULL,
    status TEXT NOT NULL,
    saved_revision BIGINT NOT NULL,
    services TEXT NOT NULL,
    nodes TEXT NOT NULL,
    namespace TEXT NOT NULL,
    run TEXT NOT NULL,
    credentials TEXT NOT NULL,
    upload TEXT NOT NULL,
    cluster_domain TEXT,
    admitted BIGINT NOT NULL,
    admitted_by TEXT,
    runner TEXT,
    lease BIGINT NOT NULL DEFAULT 0,
    started BIGINT,
    ended BIGINT,
    message TEXT,
    UNIQUE (environment_id, number)
);

-- Applied State: each node as its latest confirmed Deployment applied it.
CREATE TABLE config_applied (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    node_type TEXT NOT NULL,
    node TEXT NOT NULL,
    PRIMARY KEY (environment_id, node_id)
);

-- Registry credentials: each Service identity's, sealed, outside Working State.
CREATE TABLE config_registry_credential (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    credential TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);

-- Build receipts: the latest image each Service of an Environment built.
CREATE TABLE config_build_receipt (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    receipt TEXT NOT NULL,
    PRIMARY KEY (environment_id, service)
);

-- Builds: each Git Service a Deployment builds, pinned to one commit (none for an
-- upload), with its progress and log, and the GitHub run it was handed to.
CREATE TABLE config_build (
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    commit_sha TEXT,
    status TEXT NOT NULL,
    message TEXT,
    log TEXT NOT NULL,
    github TEXT,
    PRIMARY KEY (deployment_id, service)
);

-- Deployment Policy: each Service identity's auto-deploy, wait-for-CI, watch paths
-- and Preferred Builder, outside Working State.
CREATE TABLE config_service_policy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    policy TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);

-- The head Cloud last observed of each branch; none once deleted.
CREATE TABLE config_branch (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    head TEXT,
    PRIMARY KEY (organization_id, repository_id, branch)
);

-- Each check suite's latest result.
CREATE TABLE config_check_suite (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    suite BIGINT NOT NULL,
    head TEXT NOT NULL,
    status TEXT NOT NULL,
    conclusion TEXT,
    updated TEXT NOT NULL,
    PRIMARY KEY (organization_id, repository_id, suite)
);

-- What a push selected in an Environment while its CI runs, with the frozen
-- Conditional Saves it carries; replaced by the branch's next head.
CREATE TABLE config_waiting_deploy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    head TEXT NOT NULL,
    services TEXT NOT NULL,
    saves TEXT NOT NULL,
    PRIMARY KEY (environment_id, repository_id, branch)
);

-- Branches: an Environment made from a Parent in the same Project. Its base is
-- what it and its Parent last shared, kept means it outlives a Save, each Setup
-- Command runs in the Own Copy of one Service lineage, and closing means the
-- Store is taking it off the Servers to delete it.
CREATE TABLE config_environment_branch (
    environment_id TEXT PRIMARY KEY REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    parent_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    kept BIGINT NOT NULL,
    base TEXT NOT NULL,
    setup TEXT NOT NULL,
    closing BIGINT NOT NULL DEFAULT 0
);

-- The Organization's Build Order; none is Auto.
CREATE TABLE config_build_order (
    organization_id TEXT PRIMARY KEY,
    build_order TEXT NOT NULL
);

-- PR plans: each Project's PR Environment settings for one GitHub repository.
CREATE TABLE config_pr_plan (
    project_id TEXT NOT NULL REFERENCES config_project (id) ON DELETE CASCADE,
    repository_id BIGINT NOT NULL,
    organization_id TEXT NOT NULL,
    repository TEXT NOT NULL,
    plan TEXT NOT NULL,
    PRIMARY KEY (project_id, repository_id)
);

-- The latest facts Cloud read of each pull request, ordered by GitHub's updated_at.
CREATE TABLE config_pull_request (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    facts TEXT NOT NULL,
    updated TEXT NOT NULL,
    PRIMARY KEY (organization_id, repository_id, number)
);

-- PR Environments: the Branch the Store made for one pull request.
CREATE TABLE config_pr_environment (
    environment_id TEXT PRIMARY KEY REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL
);

-- Conditional Saves: a PR Environment's picked changes for one Destination
-- (`environment_id`), waiting to go live with its pull request's merge. `standing`
-- while the PR Environment and its target branch are as saved; `frozen` at the
-- merge, with its merge commit, no longer tied to the PR Environment; `landed`
-- with only the rows the Destination had changed too, until its next Saved
-- revision. `saved` holds the rows, core's picks and what landing needs, sealed
-- values included. A standing save goes with its PR Environment.
CREATE TABLE config_conditional_save (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    pr_environment_id TEXT REFERENCES config_environment (id) ON DELETE CASCADE,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    target_branch TEXT NOT NULL,
    working_revision BIGINT NOT NULL,
    merge_commit TEXT,
    saved_at BIGINT NOT NULL,
    saved TEXT NOT NULL
);
