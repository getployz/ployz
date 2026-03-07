// Syncs local Docker container state to the corrosion `workloads` table.
//
// Responsibilities:
// - Listen to Docker event stream (start/stop/die/destroy)
// - 100ms debounce to batch rapid events
// - Upsert workload rows to corrosion on change
// - Full sync on startup: docker ps -> reconcile with corrosion
//
// Only syncs containers with ployz labels (ployz.namespace, ployz.service, ployz.workload-id).
// System containers (wireguard, corrosion) are excluded — they have namespace "system"
// but are managed by setup.rs, not the sync loop.
