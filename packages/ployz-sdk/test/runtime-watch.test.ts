import assert from "node:assert/strict";
import test from "node:test";

import { PloyzClient, PloyzNatsTransport, PloyzNatsTransportError } from "../src/index.ts";
import type {
  PloyzNatsMessage,
  PloyzNatsRequestConnection,
  PloyzNatsResponseMessage,
  PloyzNatsStatus,
  PloyzNatsSubscription,
  RuntimeSnapshot,
} from "../src/index.ts";

test("runtime watch starts with a raw seed and replaces it with broadcasts", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(1) });
  nats.subscription.push(message(snapshot(2)));
  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(2) });
  assert.equal(nats.subject, "plz.v1.projection.runtime.snapshot");
  assert.deepEqual(nats.requestSubjects, ["plz.v1.rpc.operator.query.runtime.snapshot.seed"]);
  assert.deepEqual(JSON.parse(new TextDecoder().decode(nats.requestPayloads[0])), {});
  await iterator.return?.();
});

test("slow runtime watch consumers receive only the latest snapshot", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();

  nats.subscription.push(message(snapshot(2)));
  nats.subscription.push(message(snapshot(3)));
  await tick();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(3) });
  await iterator.return?.();
});

test("returning from runtime watch unsubscribes", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();

  await iterator.return?.();

  assert.equal(nats.subscription.unsubscribed, true);
  assert.equal(nats.statuses.returned, true);
});

test("runtime watch retries transient reconnect seed failures until success", async () => {
  const nats = new WatchNatsConnection([
    snapshot(1),
    new Error("temporarily disconnected"),
    snapshot(4),
  ]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();

  nats.statuses.push({ type: "reconnect" });

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(4) });
  assert.equal(nats.requests, 3);
  await iterator.return?.();
});

test("runtime watch retries a transient initial seed failure", async () => {
  const nats = new WatchNatsConnection([new Error("temporarily disconnected"), snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(1) });
  assert.equal(nats.requests, 2);
  await iterator.return?.();
});

test("runtime watch retries transient seed service errors", async () => {
  const nats = new WatchNatsConnection([serviceErrorResponse(503), snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(1) });
  assert.equal(nats.requests, 2);
  await iterator.return?.();
});

test("runtime stream snapshot ends initial seed retry", async () => {
  const nats = new WatchNatsConnection([new Error("temporarily disconnected")]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  const first = iterator.next();
  await tick();

  nats.subscription.push(message(snapshot(2)));

  assert.deepEqual(await first, { done: false, value: snapshot(2) });
  assert.equal(nats.requests, 1);
  await iterator.return?.();
});

test("runtime watch drops a stream snapshot buffered before a fresher seed", async () => {
  const seed = deferred<RuntimeSnapshot>();
  const nats = new WatchNatsConnection([seed.promise]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  const first = iterator.next();
  await tick();
  nats.subscription.push(message(snapshot(1)));
  seed.resolve(snapshot(2));

  assert.deepEqual(await first, { done: false, value: snapshot(2) });
  nats.subscription.push(message(snapshot(3)));
  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(3) });
  await iterator.return?.();
});

test("returning from runtime watch interrupts reconnect retry", async () => {
  const nats = new WatchNatsConnection([snapshot(1), new Error("temporarily disconnected")]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();
  nats.statuses.push({ type: "reconnect" });
  await tick();

  await iterator.return?.();
  const requestsAfterReturn = nats.requests;
  await delay(150);

  assert.equal(nats.requests, requestsAfterReturn);
  assert.equal(nats.subscription.unsubscribed, true);
});

test("returning from runtime watch interrupts the initial seed retry", async () => {
  const pendingSeed = deferred<RuntimeSnapshot>();
  const nats = new WatchNatsConnection([pendingSeed.promise]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  const first = iterator.next();
  await tick();

  const returning = iterator.return?.();
  const [firstResult, returnResult] = await Promise.race([
    Promise.all([first, returning]),
    delay(100).then(() => assert.fail("runtime watch cancellation timed out")),
  ]);

  assert.equal(firstResult.done, true);
  assert.equal(returnResult?.done, true);
  assert.equal(nats.requests, 1);
  assert.equal(nats.subscription.unsubscribed, true);
});

test("malformed runtime projection JSON terminates the watch", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();
  nats.subscription.push({ data: new TextEncoder().encode("not json") });

  await assert.rejects(
    iterator.next(),
    (error: unknown) =>
      error instanceof PloyzNatsTransportError && error.failure.kind === "decode_response",
  );
  assert.equal(nats.subscription.unsubscribed, true);
});

test("runtime watch propagates terminal NATS status errors", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats))
    .watchRuntime()
    [Symbol.asyncIterator]();
  await iterator.next();
  const permissionError = new Error("subscription permission violation");

  nats.statuses.push({ type: "error", error: permissionError });

  await assert.rejects(iterator.next(), permissionError);
  assert.equal(nats.subscription.unsubscribed, true);
});

class WatchNatsConnection implements PloyzNatsRequestConnection {
  readonly subscription = new PushSource<PloyzNatsMessage>();
  readonly statuses = new PushSource<PloyzNatsStatus>();
  readonly seeds: Array<
    RuntimeSnapshot | Error | Promise<RuntimeSnapshot> | PloyzNatsResponseMessage
  >;
  readonly requestSubjects: string[] = [];
  readonly requestPayloads: Uint8Array[] = [];
  requests = 0;
  subject?: string;

  constructor(
    seeds: Array<RuntimeSnapshot | Error | Promise<RuntimeSnapshot> | PloyzNatsResponseMessage>,
  ) {
    this.seeds = [...seeds];
  }

  async request(subject: string, payload?: Uint8Array | string): Promise<PloyzNatsResponseMessage> {
    this.requests += 1;
    this.requestSubjects.push(subject);
    assert.ok(payload instanceof Uint8Array);
    this.requestPayloads.push(payload);
    const seed = this.seeds.shift();
    assert.ok(seed);
    if (seed instanceof Error) throw seed;
    if ("data" in seed) return seed;
    return jsonResponse(await seed);
  }

  subscribe(subject: string): PloyzNatsSubscription {
    this.subject = subject;
    return this.subscription;
  }

  status(): AsyncIterable<PloyzNatsStatus> {
    return this.statuses;
  }

  closed(): Promise<void | Error> {
    return new Promise(() => undefined);
  }
}

class PushSource<T> implements AsyncIterable<T>, AsyncIterator<T> {
  readonly values: T[] = [];
  readonly waiters: Array<(result: IteratorResult<T>) => void> = [];
  returned = false;
  unsubscribed = false;

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return this;
  }

  next(): Promise<IteratorResult<T>> {
    const value = this.values.shift();
    if (value !== undefined) return Promise.resolve({ done: false, value });
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  push(value: T): void {
    const waiter = this.waiters.shift();
    if (waiter) waiter({ done: false, value });
    else this.values.push(value);
  }

  async return(): Promise<IteratorResult<T>> {
    this.returned = true;
    for (const waiter of this.waiters.splice(0)) waiter({ done: true, value: undefined });
    return { done: true, value: undefined };
  }

  unsubscribe(): void {
    this.unsubscribed = true;
    void this.return();
  }
}

function snapshot(updatedAt: number): RuntimeSnapshot {
  return {
    public_url: { mode: "none" },
    certificate_statuses: [],
    machines: [],
    services: [],
    routes: [],
    containers: [],
    revisions: [],
    releases: [],
    instances: [],
    projection_sources: {} as RuntimeSnapshot["projection_sources"],
    updated_at_unix_seconds: updatedAt,
  };
}

function message(value: unknown): PloyzNatsMessage {
  return { data: new TextEncoder().encode(JSON.stringify(value)) };
}

function jsonResponse(value: unknown): PloyzNatsResponseMessage {
  return { data: new TextEncoder().encode(JSON.stringify(value)) };
}

function serviceErrorResponse(code: 500 | 503 | 504): PloyzNatsResponseMessage {
  return {
    data: new Uint8Array(),
    headers: {
      get(name: string): string {
        if (name === "Nats-Service-Error") return "temporarily unavailable";
        if (name === "Nats-Service-Error-Code") return String(code);
        return "";
      },
    },
  };
}

function tick(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolvePromise: (value: T) => void = () => undefined;
  const promise = new Promise<T>((resolve) => {
    resolvePromise = resolve;
  });
  return { promise, resolve: resolvePromise };
}
