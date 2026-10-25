"use strict";

// ponytail: the specifier is computed so bundlers cannot follow require into the
// .node binary. Nitro/Vinxi emit this file as ESM without CJS module globals.
const native = require([".", "ployz-sdk.node"].join("/"));

const { RpcError, throwRpcError, withRpcError, rpc, forward, bindAll } = require("./wrap.js");
const runtimeLogs = require("./runtime-logs.js");

class Client {
  constructor(inner) {
    this._inner = inner;
    const logTransport = {
      watch: (options) => iterateWatch(() => inner.watch(), options.signal),
      open: async (input, sealed) => {
        const reader = await withRpcError(inner.containerLogs(input, sealed));
        return { next: () => withRpcError(reader.next()), cancel: () => reader.cancel() };
      },
      history: async (input, sealed) => {
        const reader = await withRpcError(inner.logHistory(input, sealed));
        return { next: () => withRpcError(reader.next()), cancel: () => reader.cancel() };
      },
    };
    this.runtime = {
      watch: (options = {}) => iterateWatch(() => inner.watch(), options && options.signal),
      logs: (options = {}) => runtimeLogs.logs(logTransport, options),
      logHistory: (options) => runtimeLogs.history(logTransport, options),
      metricHistory: (input) => withRpcError(inner.metricHistory(input)),
    };
  }

  prepare(input, options = {}) {
    return wrapProgress(() => this._inner.prepare(input), options.signal, wrapPreview);
  }

  build(input, options = {}) {
    return wrapProgress(() => this._inner.build(input, options.startWithinMs), options.signal);
  }

  async preview(intent) {
    return wrapPreview(await withRpcError(this._inner.preview(intent)));
  }

  async previewNamespaceRemoval(namespace, destroyVolumes) {
    return wrapPreview(
      await withRpcError(this._inner.previewNamespaceRemoval(namespace, destroyVolumes)),
    );
  }

  async run(intent, options = {}) {
    const preview = await this.preview(intent);
    const running = preview.confirm(options);
    return running.finished;
  }

  dataLossIfNamespaceDestroyed(namespace, destroyVolumes = false) {
    return withRpcError(this._inner.dataLossIfNamespaceDestroyed(namespace, destroyVolumes));
  }

  destroyNamespace(namespace, confirmDataLoss, destroyVolumes = false) {
    return withRpcError(this._inner.destroyNamespace(namespace, confirmDataLoss, destroyVolumes));
  }
}

forward(Client, native.Client, ["containerLogs", "logHistory", "metricHistory", "watch"]);

function wrapProgress(start, signal, wrap = value => value) {
  signal?.throwIfAborted();
  let pending;
  try { pending = start(); } catch (error) { throwRpcError(error); }
  const stop = () => pending.abort();
  signal?.addEventListener("abort", stop, { once: true });
  const finished = withRpcError(pending.finished()).then(wrap)
    .finally(() => signal?.removeEventListener("abort", stop));
  // Callers may consume progress before awaiting the terminal result.
  void finished.catch(() => {});
  return {
    abort: stop,
    finished,
    async *[Symbol.asyncIterator]() {
      for (;;) {
        const value = await withRpcError(pending.next());
        if (value == null) return;
        yield value;
      }
    },
  };
}

function wrapPreview(handle) {
  const payload = handle.payload();
  return {
    ...payload,
    noop: payload.operations.length === 0,
    buildReceipts: handle.buildReceipts(),
    pruneTargets: handle.pruneTargets(),
    close: () => handle.close(),
    confirm(options = {}) {
      try {
        return wrapRunning(handle.confirm(options?.deploymentId, options?.imageCleanup), options && options.signal);
      } catch (error) {
        throwRpcError(error);
      }
    },
  };
}

function wrapRunning(running, signal) {
  const stop = () => running.abort();
  if (signal?.aborted) {
    stop();
  } else if (signal) {
    signal.addEventListener("abort", stop, { once: true });
  }
  let finished;
  return {
    abort: stop,
    get finished() {
      finished ??= withRpcError(running.finished());
      return finished;
    },
    async *[Symbol.asyncIterator]() {
      try {
        for (;;) {
          const value = await running.next();
          if (value == null) {
            return;
          }
          yield value;
        }
      } finally {
        if (signal) {
          signal.removeEventListener("abort", stop);
        }
      }
    },
  };
}

async function* iterateWatch(start, signal) {
  if (signal?.aborted) {
    return;
  }
  let stream;
  const stop = () => {
    if (stream) {
      stream.cancel();
    }
  };
  if (signal) {
    signal.addEventListener("abort", stop, { once: true });
  }
  try {
    stream = await withRpcError(start());
    if (signal?.aborted) {
      stream.cancel();
      return;
    }
    for (;;) {
      const value = await withRpcError(stream.next());
      if (value == null || signal?.aborted) {
        return;
      }
      yield value;
    }
  } finally {
    if (signal) {
      signal.removeEventListener("abort", stop);
    }
    stop();
  }
}

function defaultPlanOptions() {
  return {
    force_recreate: false,
    skip_health_monitor: false,
    placement_seed: 0,
    selected: [],
  };
}

function applyAll(namespace, specs, options = defaultPlanOptions()) {
  return {
    namespace,
    target: specs,
    options,
  };
}

function applyOne(namespace, spec, options = defaultPlanOptions()) {
  return {
    namespace,
    target: [spec],
    options: {
      ...defaultPlanOptions(),
      ...options,
      selected: [{ name: spec.name }],
    },
  };
}

// Cloud's close budget for a short-lived session (SESSION_CLOSE_CLUSTER_MS), which this package cannot import.
const STOPPED_CLOSE_MS = 2_000;

function settledWithin(promise, ms) {
  let timer;
  const elapsed = new Promise(resolve => (timer = setTimeout(resolve, ms)));
  return Promise.race([promise, elapsed]).finally(() => clearTimeout(timer));
}

function safeDialTiming(timing) {
  if (timing == null || typeof timing !== "object") return undefined;
  const outcome = ["answered", "cancelled", "timed_out", "refused", "unanswered"].includes(timing.outcome)
    ? timing.outcome : "unanswered";
  const pathKind = ["direct", "relayed", "unknown"].includes(timing.pathKind) ? timing.pathKind : "unknown";
  const safe = { outcome, pathKind };
  if (typeof timing.correlation === "string" && /^[0-9a-f]{16}$/.test(timing.correlation)) safe.correlation = timing.correlation;
  if (["sdk_deadline", "caller"].includes(timing.stoppedBy)) safe.stoppedBy = timing.stoppedBy;
  for (const field of ["totalMs", "endpointBindMs", "irohConnectMs", "proofMs", "http2ChannelMs", "probeDescribeContractMs", "entryDescribeContractMs"]) {
    if (Number.isSafeInteger(timing[field]) && timing[field] >= 0) safe[field] = timing[field];
  }
  const unfinished = ["endpoint_bind", "iroh_connect", "proof", "http2_channel", "probe_describe_contract", "entry_describe_contract"];
  if (unfinished.includes(timing.unfinishedStage)) safe.unfinishedStage = timing.unfinishedStage;
  return Object.freeze(safe);
}

function readDialTiming(attempt) {
  try { return safeDialTiming(attempt.dialTimings?.()); } catch { return undefined; }
}

function reportDialTiming(callback, timing) {
  if (timing == null) return;
  try {
    const returned = callback?.(timing);
    if (returned != null && typeof returned.then === "function") void Promise.resolve(returned).catch(() => {});
  } catch {}
}

async function connect(options) {
  const { signal, timeoutMs, onDialTiming } = options;
  if (timeoutMs !== undefined && (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2147483647)) {
    throw new TypeError("timeoutMs must be a positive 32-bit integer");
  }
  signal?.throwIfAborted();
  let attempt;
  try { attempt = native.startConnections(options.connections, options.sampleDialTiming, options.dialCorrelation); } catch (error) { throwRpcError(error); }
  let client;
  let timer;
  let stopped = false;
  let deadlineError;
  // The caller's own close reports a failed close; this one has nobody awaiting it.
  const stop = (timedOut) => { stopped = true; attempt.cancel(timedOut); void client?.close().catch(() => {}); };
  const abort = () => stop(false);
  const cleanup = () => { clearTimeout(timer); signal?.removeEventListener("abort", abort); };
  signal?.addEventListener("abort", abort, { once: true });
  if (timeoutMs !== undefined) timer = setTimeout(() => stop(true), timeoutMs);
  try {
    client = new Client(await withRpcError(attempt.wait()));
    const timing = readDialTiming(attempt);
    if (timing != null) client.dialTiming = timing;
    const close = client.close.bind(client);
    // Keep listening until the close finishes, so an abort during it still reaches the session.
    client.close = async () => { try { return await close(); } finally { cleanup(); } };
    // The abort or deadline is why connect failed, so a failed or hung close here must not replace or hold it.
    if (stopped) {
      await settledWithin(client.close().catch(() => {}), STOPPED_CLOSE_MS);
      signal?.throwIfAborted();
      deadlineError = new Error("session deadline exceeded");
      throw deadlineError;
    }
    reportDialTiming(onDialTiming, client.dialTiming);
    return client;
  } catch (error) {
    const timing = readDialTiming(attempt);
    reportDialTiming(onDialTiming, timing);
    if (timing != null && error != null && error !== signal?.reason && (error instanceof RpcError || error === deadlineError)) {
      try { error.dialTiming = timing; } catch {}
    }
    cleanup();
    throw error;
  }
}

async function openConfigStore(url, sealingSecret) {
  return bindAll(await withRpcError(native.openConfigStore(url, sealingSecret)), native.ConfigStore);
}

const observeVolumes = rpc(native.observeVolumes);
const observeCopies = rpc(native.observeCopies);
const observeRunning = rpc(native.observeRunning);
const allocateEnrollment = rpc(native.allocateEnrollment);
const machineUpgradeAttemptId = rpc(native.machineUpgradeAttemptId);
const ployzVersion = rpc(native.ployzVersion);
const agentTools = rpc(native.agentTools);
const agentStart = rpc(native.agentStart);
const agentResume = rpc(native.agentResume);
const agentCloudCall = rpc(native.agentCloudCall);
const { SealedNames } = native;

function setVolumeCopyRefusal(error) {
  const details = error?.details;
  if (details == null) return null;
  try {
    return native.setVolumeCopyRefusal(details);
  } catch {
    return null;
  }
}

// Shorthand entries only: Node's CommonJS export lexer stops at the first non-identifier value, and every name it
// misses is unavailable to `import { name } from "@ployz/sdk"`.
module.exports = {
  openConfigStore,
  observeVolumes,
  observeCopies,
  observeRunning,
  allocateEnrollment,
  setVolumeCopyRefusal,
  machineUpgradeAttemptId,
  ployzVersion,
  agentTools,
  agentStart,
  agentResume,
  agentCloudCall,
  connect,
  Client,
  RpcError,
  SealedNames,
  applyAll,
  applyOne,
};
