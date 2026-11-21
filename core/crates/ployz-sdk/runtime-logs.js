"use strict";

function matches(container, filter = {}) {
  return (!filter.namespace || container.namespace === filter.namespace)
    && (!filter.serviceId || container.labels["cloud.ployz.service.id"] === filter.serviceId)
    && (!filter.serviceName || container.resolved_spec.name === filter.serviceName)
    && (!filter.machineId || container.machine_id === filter.machineId)
    && (!filter.containerId || container.container_id === filter.containerId)
    && (!filter.kind || container.kind === filter.kind)
    && (!filter.deploymentId || container.labels["ployz.deployment.id"] === filter.deploymentId);
}
function sourceKey(container) { return `${container.machine_id}/${container.container_id}`; }
function recordKey(record) {
  return `${record.source.machine_id}/${record.source.origin.container_id}/${record.timestamp_nanos}`;
}
function identify(record, counts) {
  const group = recordKey(record);
  const ordinal = counts.get(group) ?? 0;
  counts.set(group, ordinal + 1);
  return { ...record, id: `${group}/${String(ordinal).padStart(12, "0")}` };
}
function compare(a, b) {
  const time = BigInt(a.timestamp_nanos) - BigInt(b.timestamp_nanos);
  return time < 0n ? -1 : time > 0n ? 1 : a.id.localeCompare(b.id);
}
function failure(error) {
  const capability = error.details?.capability;
  return {
    message: error.message,
    ...(typeof error.code === "string" && { code: error.code }),
    ...(typeof capability === "string" && { capability }),
  };
}
function input(container, tail, follow) {
  return { machine_id: container.machine_id, container_id: container.container_id, tail, follow, since_unix_seconds: null };
}

/** One Corrosion watch discovers sources; slow Machines never block other sources. */
async function* logs(transport, options = {}) {
  if (!options.sealed) throw new TypeError("sealed is required");
  if (!Number.isInteger(options.tail ?? 200) || (options.tail ?? 200) < 0 || (options.tail ?? 200) > 1000) throw new RangeError("tail must be 0..1000");
  const controller = new AbortController();
  const stop = () => controller.abort();
  options.signal?.throwIfAborted();
  options.signal?.addEventListener("abort", stop, { once: true });
  const watch = transport.watch({ signal: controller.signal })[Symbol.asyncIterator]();
  const pending = new Map();
  const sources = new Map();
  const counts = new Map();
  const nextWatch = () => pending.set("watch", watch.next().then(value => ({ key: "watch", value }), error => ({ key: "watch", error })));
  const nextLog = (key, source) => pending.set(key, source.reader.next().then(value => ({ key, value }), error => ({ key, error })));
  const openSource = (key, source) => {
    source.boundary = source.last;
    source.skip = source.last === null ? 0 : counts.get(`${key}/${source.last}`) ?? 0;
    const request = input(source.container, source.last === null ? options.tail ?? 200 : -1, options.follow !== false);
    if (source.last !== null) request.since_unix_seconds = Number(BigInt(source.last) / 1_000_000_000n);
    pending.set(key, transport.open(request, options.sealed).then(reader => {
      if (controller.signal.aborted) reader.cancel();
      return { key, opened: reader };
    }, error => ({ key, error })));
  };
  const cancel = () => { for (const source of sources.values()) source.reader?.cancel(); };
  controller.signal.addEventListener("abort", cancel, { once: true });
  try {
    nextWatch();
    while (pending.size && !controller.signal.aborted) {
      const event = await Promise.race(pending.values());
      pending.delete(event.key);
      if (event.key === "watch") {
        if (event.error) throw event.error;
        if (event.value.done) break;
        for (const container of event.value.value.containers) {
          if (!matches(container, options.filter)) continue;
          const key = sourceKey(container);
          let source = sources.get(key);
          if (!source) {
            source = { reader: null, last: null, failed: false, handoffSpent: false, container };
            sources.set(key, source);
          } else {
            if (source.container.runtime.state !== container.runtime.state) source.handoffSpent = false;
            source.container = container;
          }
          if (pending.has(key) || source.reader || source.failed) continue;
          if (source.handoffSpent && container.runtime.state !== "running") continue;
          if (source.last !== null && container.runtime.state !== "running") continue;
          openSource(key, source);
        }
        // A failed source's container missing from a complete observation has left; nothing more will come from it.
        const seen = new Set(event.value.value.containers.map(sourceKey));
        const incomplete = event.value.value.incomplete_ids ?? {};
        for (const [key, source] of sources) {
          const [machineId, containerId] = key.split("/");
          if (!source.failed || seen.has(key) || incomplete.machines?.includes(machineId) || incomplete.containers?.includes(containerId)) continue;
          sources.delete(key);
          yield { type: "source_gone", machineId, containerId };
        }
        if (options.follow !== false) nextWatch();
      } else {
        const source = sources.get(event.key);
        if (event.opened) {
          source.reader = event.opened;
          nextLog(event.key, source);
        } else if (event.error || event.value == null) {
          source.reader?.cancel(); source.reader = null;
          source.failed ||= !!event.error;
          if (!source.failed && options.follow !== false && source.container.runtime.state === "running" && !source.handoffSpent) {
            source.handoffSpent = true;
            openSource(event.key, source);
          }
          if (event.error) {
            const [machineId, containerId] = event.key.split("/");
            yield { type: "source_error", machineId, containerId, ...failure(event.error) };
          }
        } else {
          const row = event.value;
          if (row.channel === "error") {
            const [machineId, containerId] = event.key.split("/");
            source.failed = true;
            yield { type: "source_error", machineId, containerId, message: row.message };
          } else if (source.boundary === null || BigInt(row.timestamp_nanos) > BigInt(source.boundary) || row.timestamp_nanos === source.boundary && source.skip-- <= 0) {
            source.boundary = null;
            source.handoffSpent = false;
            source.last = row.timestamp_nanos;
            yield { type: "record", record: identify(row, counts) };
          }
          nextLog(event.key, source);
        }
      }
    }
  } finally {
    controller.abort();
    await watch.return?.();
    options.signal?.removeEventListener("abort", stop);
  }
}
module.exports = { logs };
