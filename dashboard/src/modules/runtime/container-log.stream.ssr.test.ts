import { QueryClient } from "@tanstack/react-query";
import { expect, it } from "vitest";
import { getContainerLogStream } from "./container-log.stream";

it("renders the loading state on the server without opening a stream", async () => {
  // Node has no EventSource: opening one here is what broke SSR of a logs tab.
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "acme", serviceId: "api" }, { queryClient, sessionId: "s", userId: "u" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    expect(stream.getSnapshot().opened).toBe(false);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear();
  }
});
