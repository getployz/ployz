"use strict";

function matches(container, filter = {}) {
  return (!filter.projectName || container.project_name === filter.projectName)
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
function input(container, tail, follow, before) {
  return { machine_id: container.machine_id, container_id: container.container_id, tail, follow, before_nanos: before ?? null, since_unix_seconds: null };
}

/** One Corrosion watch discovers sources; slow Machines never block other sources. */
async function* logs(transport, options = {}) {
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
    pending.set(key, transport.open(request).then(reader => {
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
            source = { reader: null, last: null, failed: false, reopened: false, container };
            sources.set(key, source);
          } else {
            source.reopened = false;
            source.container = container;
          }
          if (pending.has(key) || source.reader || source.failed || source.reopened) continue;
          if (source.last !== null && container.runtime.state !== "running") continue;
          openSource(key, source);
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
          // One immediate handoff covers a running observation that raced ahead
          // of EOF. Wait for a fresh observation/output before another handoff.
          if (!source.failed && options.follow !== false && source.container.runtime.state === "running" && !source.reopened) {
            source.reopened = true;
            openSource(event.key, source);
          }
          if (event.error) {
            const [machineId, containerId] = event.key.split("/");
            yield { type: "source_error", machineId, containerId, message: event.error.message };
          }
        } else {
          const row = event.value;
          if (row.channel === "error") {
            const [machineId, containerId] = event.key.split("/");
            source.failed = true;
            yield { type: "source_error", machineId, containerId, message: row.message };
          } else if (source.boundary === null || BigInt(row.timestamp_nanos) > BigInt(source.boundary) || row.timestamp_nanos === source.boundary && source.skip-- <= 0) {
            source.boundary = null;
            source.reopened = false;
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

/** Finite reads leave the viewer's live stream untouched. */
async function history(transport, options) {
  options.signal?.throwIfAborted();
  const limit = options.limit ?? 200;
  if (!Number.isInteger(limit) || limit < 1 || limit > 1000) throw new RangeError("limit must be between 1 and 1000");
  if (!options.before || Object.values(options.before).some(before => typeof before !== "string" || !/^-?\d+$/.test(before))) throw new TypeError("before must map log sources to nanosecond timestamps");
  const watch = transport.watch({ signal: options.signal })[Symbol.asyncIterator]();
  let containers;
  try { containers = (await watch.next()).value?.containers ?? []; }
  finally { await watch.return?.(); }
  const results = await Promise.all(containers.filter(container => matches(container, options.filter) && options.before[sourceKey(container)] !== undefined).map(async container => {
    let reader;
    const cancel = () => reader?.cancel();
    options.signal?.addEventListener("abort", cancel, { once: true });
    try {
      reader = await transport.open(input(container, limit, false, options.before[sourceKey(container)]));
      options.signal?.throwIfAborted();
      const records = []; const counts = new Map();
      for (;;) { const row = await reader.next(); if (!row) break; if (row.channel === "error") throw new Error(row.message); records.push(identify(row, counts)); }
      return { records, errors: [] };
    } catch (error) {
      options.signal?.throwIfAborted();
      return { records: [], errors: [{ type: "source_error", machineId: container.machine_id, containerId: container.container_id, message: error.message }] };
    } finally { cancel(); options.signal?.removeEventListener("abort", cancel); }
  }));
  const all = results.flatMap(result => result.records).sort(compare);
  return { records: all, errors: results.flatMap(result => result.errors) };
}
module.exports = { logs, history };
