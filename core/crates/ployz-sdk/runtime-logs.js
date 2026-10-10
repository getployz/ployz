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
function input(container, tail, follow) {
  return { machine_id: container.machine_id, container_id: container.container_id, tail, follow, since_unix_seconds: null };
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

const HISTORY_LIMIT = 5000;
const KINDS = { service_container: "service", pre_deploy_hook: "pre_deploy_hook" };
const later = (a, b) => (BigInt(a) > BigInt(b) ? a : b);
const earlier = (a, b) => (BigInt(a) < BigInt(b) ? a : b);
const rowTime = row => row.row === "gap" ? row.from_nanos : row.timestamp_nanos;

function encodeCursor(states) {
  return Object.keys(states).length ? Buffer.from(JSON.stringify(states)).toString("base64url") : null;
}
function decodeCursor(cursor) {
  try {
    const states = JSON.parse(Buffer.from(cursor, "base64url").toString());
    if (states && typeof states === "object" && !Array.isArray(states)) return states;
  } catch {}
  throw new TypeError("cursor did not come from a history page");
}

function selector(filter, containers) {
  const live = filter.serviceId === undefined ? undefined
    : containers.find(container => container.labels["cloud.ployz.service.id"] === filter.serviceId);
  const service = filter.serviceName ?? (live && (live.labels["ployz.service.name"] ?? live.resolved_spec.name));
  if (filter.serviceId !== undefined && service === undefined) return null;
  return { namespace: filter.namespace, service, deployment: filter.deploymentId, container_id: filter.containerId };
}

async function readPage(transport, machineId, chosen, state, limit, signal) {
  const reader = await transport.history({ machine_id: machineId, ...chosen, direction: "backward", limit, cursor: state.c ?? null, until_nanos: state.u ?? null });
  const cancel = () => reader.cancel();
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    const containers = new Map(); const rows = [];
    for (;;) {
      signal?.throwIfAborted();
      const row = await reader.next();
      if (!row) throw new Error("the Server's Log Store ended a page early");
      if (row.row === "end") return { rows, next: row.next };
      if (row.row === "container") containers.set(row.container_id, row);
      else rows.push({ ...row, container: containers.get(row.container_id) });
    }
  } finally { cancel(); signal?.removeEventListener("abort", cancel); }
}

/**
 * One page of every Server's Log Store, newest first. Each Server reads its own newest page; the page stops at the
 * newest of their oldest rows, so a Server that went further back than another doesn't skip what the other has
 * left. A Server that fails is named and asked again on the next page.
 */
async function history(transport, options) {
  options.signal?.throwIfAborted();
  const limit = options.limit ?? 200;
  if (!Number.isInteger(limit) || limit < 1 || limit > HISTORY_LIMIT) throw new RangeError(`limit must be between 1 and ${HISTORY_LIMIT}`);
  const filter = options.filter ?? {};
  const watch = transport.watch({ signal: options.signal })[Symbol.asyncIterator]();
  let view;
  try { view = (await watch.next()).value ?? { machines: [], containers: [] }; }
  finally { await watch.return?.(); }
  const page = { records: [], gaps: [], exits: [], failures: [], cursor: null };
  const chosen = selector(filter, view.containers);
  if (!chosen) return page;
  const names = new Map(view.machines.map(({ machine }) => [machine.id, machine.name]));
  const states = options.cursor === undefined
    ? Object.fromEntries(view.machines.map(({ machine }) => [machine.id, options.before === undefined ? {} : { u: options.before }]))
    : decodeCursor(options.cursor);
  const machines = Object.keys(states).filter(id => !filter.machineId || id === filter.machineId);
  const reads = await Promise.all(machines.map(machineId => readPage(transport, machineId, chosen, states[machineId], limit, options.signal)
    .then(page => ({ machineId, page }), error => { options.signal?.throwIfAborted(); return { machineId, error }; })));
  const oldest = rows => rows.reduce((min, row) => earlier(min, rowTime(row)), rowTime(rows[0]));
  const horizon = reads.filter(read => read.page?.next && read.page.rows.length)
    .reduce((max, read) => (max === null ? oldest(read.page.rows) : later(max, oldest(read.page.rows))), null);
  const next = {};
  for (const read of reads) {
    const machineName = names.get(read.machineId) ?? read.machineId;
    if (read.error) {
      page.failures.push({ machineId: read.machineId, machineName, message: read.error.message });
      next[read.machineId] = states[read.machineId];
      continue;
    }
    const { rows } = read.page;
    const whole = read.page.next && (!rows.length || oldest(rows) === horizon);
    const kept = whole || horizon === null ? rows : rows.filter(row => BigInt(rowTime(row)) >= BigInt(horizon));
    if (whole) next[read.machineId] = { c: read.page.next };
    else if (kept.length < rows.length || read.page.next) next[read.machineId] = { u: horizon };
    const state = states[read.machineId];
    const counts = new Map(state.t === undefined ? [] : [[state.t, state.n]]);
    for (const row of kept) {
      const time = rowTime(row);
      const ordinal = counts.get(time) ?? 0;
      counts.set(time, ordinal + 1);
      const container = row.container ?? { kind: "service", service: null, replica: "", namespace: null };
      if (filter.kind && KINDS[filter.kind] !== container.kind) continue;
      const serviceName = container.service ?? container.replica;
      const source = { machineId: read.machineId, machineName, containerId: row.container_id, serviceName };
      if (row.row === "gap") page.gaps.push({ ...source, fromNanos: row.from_nanos, toNanos: row.to_nanos, reason: row.reason });
      else if (row.row === "exit") page.exits.push({ ...source, timestampNanos: row.timestamp_nanos, exitCode: row.exit_code, oomKilled: row.oom_killed });
      else {
        const group = `${read.machineId}/${row.container_id}/${row.timestamp_nanos}`;
        const live = view.containers.find(candidate => candidate.container_id === row.container_id);
        page.records.push({
          id: `${group}/history/${ordinal}`,
          source: {
            origin: { origin: "service", service_id: live?.labels["cloud.ployz.service.id"] ?? filter.serviceId ?? "", service_name: serviceName, container_id: row.container_id, hook: container.kind === "pre_deploy_hook" ? container.replica : null },
            machine_id: read.machineId, machine_name: machineName,
          },
          timestamp_nanos: row.timestamp_nanos, channel: row.stream, level: row.level, message: row.message,
        });
      }
    }
    if (whole) {
      const time = rows.length ? oldest(rows) : state.t;
      if (time !== undefined) Object.assign(next[read.machineId], { t: time, n: counts.get(time) });
    }
  }
  page.records.sort((a, b) => -compare(a, b));
  page.cursor = encodeCursor(next);
  return page;
}
module.exports = { logs, history };
