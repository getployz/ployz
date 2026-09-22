// @vitest-environment jsdom
import { expect, it, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { Schema } from "effect";
import { preloadCollection } from "#/collections/query-collection";
import { deploymentOperationEvidencePageQuerySchema } from "./deployment-contract";
import type { listDeploymentProgressLogsServerFn } from "./deployment.functions";
import { createDeploymentLogsCollection } from "./deployment-log.collection";


it("loads the first log page through the API contract and follows its cursor", async () => {
  const client = new QueryClient();
  const deploymentId = "8f79e99b-cd08-4e9c-af96-f3fed313acc5";
  const output = "Railpack could not determine how to build the app.";
  const row = (id: number) => ({ id, deploymentId, createdAt: new Date(), progress: {
    completed: 0, total: 0, outcome: null, rows: [], compensation: [],
    preparation: { phase: "build" as const, serviceId: null, machineId: null,
      machineName: null, message: null, output, outputTruncated: false },
  } });
  const readPage = vi.fn(async ({ data }: Parameters<typeof listDeploymentProgressLogsServerFn>[0]) => {
    const query = Schema.decodeUnknownSync(deploymentOperationEvidencePageQuerySchema)(data);
    return query.afterSequence === undefined
      ? { finished: true, events: [row(1)], nextSequence: "1" }
      : { finished: true, events: [row(2)], nextSequence: null };
  });
  const collection = createDeploymentLogsCollection("nick", deploymentId, {
    queryClient: client, sessionId: "session", userId: "user",
  }, readPage);
  try {
    await preloadCollection(collection);
    expect(Array.from(collection.values()).map((event) => event.progress.preparation?.output)).toEqual([output, output]);
    expect(readPage.mock.calls.map(([request]) => request.data.afterSequence)).toEqual([undefined, "1"]);
  } finally {
    await collection.cleanup();
    client.clear();
  }
});

it("polls an active deployment through its final output, then stops", async () => {
  vi.useFakeTimers();
  const client = new QueryClient();
  const readPage = vi.fn(async () => ({ events: [], nextSequence: null, finished: false }));
  const collection = createDeploymentLogsCollection("nick", "8f79e99b-cd08-4e9c-af96-f3fed313acc5", {
    queryClient: client, sessionId: "poll-session", userId: "user",
  }, readPage);
  const observer = collection.subscribeChanges(() => {});
  try {
    await collection.preload();
    expect(readPage).toHaveBeenCalledTimes(1);
    readPage.mockResolvedValue({ events: [], nextSequence: null, finished: true });
    await vi.advanceTimersByTimeAsync(2_000);
    expect(readPage).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(readPage).toHaveBeenCalledTimes(2);
  } finally {
    observer.unsubscribe();
    await collection.cleanup(); client.clear(); vi.useRealTimers();
  }
});
