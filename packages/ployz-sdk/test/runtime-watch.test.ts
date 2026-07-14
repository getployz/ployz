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

test("runtime watch starts with a snapshot and replaces it with broadcasts", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(1) });
  nats.subscription.push(message(snapshot(2)));
  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(2) });
  assert.equal(nats.subject, "plz.v1.projection.runtime.snapshot");
  await iterator.return?.();
});

test("slow runtime watch consumers receive only the latest snapshot", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
  await iterator.next();

  nats.subscription.push(message(snapshot(2)));
  nats.subscription.push(message(snapshot(3)));
  await tick();

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(3) });
  await iterator.return?.();
});

test("returning from runtime watch unsubscribes", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
  await iterator.next();

  await iterator.return?.();

  assert.equal(nats.subscription.unsubscribed, true);
  assert.equal(nats.statuses.returned, true);
});

test("runtime watch requests a fresh seed after reconnect", async () => {
  const nats = new WatchNatsConnection([snapshot(1), snapshot(4)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
  await iterator.next();

  nats.statuses.push({ type: "reconnect" });

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(4) });
  assert.equal(nats.requests, 2);
  await iterator.return?.();
});

test("runtime watch survives a transient reconnect seed failure", async () => {
  const nats = new WatchNatsConnection([snapshot(1), new Error("temporarily disconnected")]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
  await iterator.next();

  nats.statuses.push({ type: "reconnect" });
  await tick();
  nats.subscription.push(message(snapshot(2)));

  assert.deepEqual(await iterator.next(), { done: false, value: snapshot(2) });
  await iterator.return?.();
});

test("malformed runtime projection JSON terminates the watch", async () => {
  const nats = new WatchNatsConnection([snapshot(1)]);
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
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
  const iterator = new PloyzClient(new PloyzNatsTransport(nats)).watchRuntime()[Symbol.asyncIterator]();
  await iterator.next();
  const permissionError = new Error("subscription permission violation");

  nats.statuses.push({ type: "error", error: permissionError });

  await assert.rejects(iterator.next(), permissionError);
  assert.equal(nats.subscription.unsubscribed, true);
});

class WatchNatsConnection implements PloyzNatsRequestConnection {
  readonly subscription = new PushSource<PloyzNatsMessage>();
  readonly statuses = new PushSource<PloyzNatsStatus>();
  readonly seeds: Array<RuntimeSnapshot | Error>;
  requests = 0;
  subject?: string;

  constructor(seeds: Array<RuntimeSnapshot | Error>) {
    this.seeds = [...seeds];
  }

  async request(): Promise<PloyzNatsResponseMessage> {
    this.requests += 1;
    const seed = this.seeds.shift();
    assert.ok(seed);
    if (seed instanceof Error) throw seed;
    return jsonResponse({ status: "ok", value: { snapshot: seed } });
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
    machines: [], services: [], routes: [], containers: [], revisions: [], releases: [], instances: [],
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

function tick(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}
