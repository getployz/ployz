import { createServer } from "node:http";
import { expect, it } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { createApiCollection, createChangeCollection, preloadCollection } from "./query-collection";
import { getDbClient } from "./scope";

it.each(["api", "change"] as const)("recovery does not outlive an SSR response's scope (%s)", async (kind) => {
  const client = new QueryClient();
  let reads = 0;
  const read = async () => {
    reads += 1;
    if (reads === 1) throw new Error("first");
    return [{ id: "one" }];
  };
  const input = { queryClient: client, queryKey: ["response", kind], getKey: (row: { id: string }) => row.id };
  const collection = kind === "api"
    ? createApiCollection({ ...input, queryFn: read })
    : createChangeCollection({ ...input, read: async () => ({ full: true as const, rows: await read(), cursor: "1" }) });
  let resolve!: (after: { status: string; reads: number }) => void;
  const after = new Promise<{ status: string; reads: number }>((settle) => { resolve = settle; });
  const server = createServer(async (_request, response) => {
    await preloadCollection(collection).catch(() => {});
    await collection.utils.refetch();
    response.end("response");
    await getDbClient(client).cleanup();
    client.clear();
    setTimeout(() => resolve({ status: collection.status, reads }), 20);
  });
  await new Promise<void>((listening) => server.listen(0, "127.0.0.1", listening));
  try {
    const { port } = server.address() as { port: number };
    expect(await (await fetch(`http://127.0.0.1:${port}`)).text()).toBe("response");
    expect(await after).toEqual({ status: "cleaned-up", reads: 2 });
  } finally {
    await collection.cleanup();
    client.clear();
    await new Promise<void>((closed) => server.close(() => closed()));
  }
});
