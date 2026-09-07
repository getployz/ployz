import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import {
  latestVolumeRemoveAttemptQueryOptions,
  rememberLatestVolumeRemoveAttempt,
  type LatestVolumeRemoveAttempt,
} from "#/modules/runtime/volume-removal.queries";

const input = {
  organizationSlug: "volumes",
  environmentId: "00000000-0000-4000-8000-000000000605",
  resourceId: "00000000-0000-4000-8000-000000000607",
};

function latestAttempt(
  overrides: Pick<LatestVolumeRemoveAttempt, "id" | "status"> &
    Partial<Pick<LatestVolumeRemoveAttempt, "failureMessage">>,
): LatestVolumeRemoveAttempt {
  return {
    id: overrides.id,
    organizationId: "00000000-0000-4000-8000-000000000601",
    requestedByUserId: "00000000-0000-4000-8000-000000000602",
    environmentId: input.environmentId,
    environmentResourceId: input.resourceId,
    retryOfAttemptId: null,
    volumes: [],
    status: overrides.status,
    inngestRunId: null,
    outcome: null,
    failureMessage: overrides.failureMessage ?? null,
    startedAt: null,
    terminalAt: null,
    createdAt: new Date("2026-09-06T00:00:00.000Z"),
    updatedAt: new Date("2026-09-06T00:00:00.000Z"),
  };
}

const pendingAttempt = latestAttempt({
  id: "00000000-0000-4000-8000-000000000701",
  status: "pending",
});

function queryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
}

describe("latest volume remove attempt cache", () => {
  it("reads the pending row after confirm fails instead of keeping a cached empty latest", async () => {
    const client = queryClient();
    let serverLatest: LatestVolumeRemoveAttempt | null = null;
    const options = latestVolumeRemoveAttemptQueryOptions(input, async () =>
      serverLatest,
    );
    client.setQueryData(options.queryKey, null);
    serverLatest = pendingAttempt;

    await expect(
      rememberLatestVolumeRemoveAttempt(client, options.queryKey, async () => {
        throw new Error("Inngest unavailable");
      }),
    ).rejects.toThrow("Inngest unavailable");

    await expect(
      client.fetchQuery({ ...options, staleTime: Infinity }),
    ).resolves.toEqual(pendingAttempt);
  });

  it("drops a cached partial after retry dispatch fails so the pending successor is visible", async () => {
    const client = queryClient();
    const partialAttempt = latestAttempt({
      id: "00000000-0000-4000-8000-000000000702",
      status: "partial",
      failureMessage: "busy",
    });
    const successorAttempt = latestAttempt({
      id: "00000000-0000-4000-8000-000000000703",
      status: "pending",
    });
    let serverLatest: LatestVolumeRemoveAttempt | null = partialAttempt;
    const options = latestVolumeRemoveAttemptQueryOptions(input, async () =>
      serverLatest,
    );
    client.setQueryData(options.queryKey, partialAttempt);
    serverLatest = successorAttempt;

    await expect(
      rememberLatestVolumeRemoveAttempt(client, options.queryKey, async () => {
        throw new Error("Inngest unavailable");
      }),
    ).rejects.toThrow("Inngest unavailable");

    await expect(
      client.fetchQuery({ ...options, staleTime: Infinity }),
    ).resolves.toEqual(successorAttempt);
  });

  it("caches the started attempt when confirm succeeds", async () => {
    const client = queryClient();
    const options = latestVolumeRemoveAttemptQueryOptions(
      input,
      async () => null,
    );
    client.setQueryData(options.queryKey, null);

    await expect(
      rememberLatestVolumeRemoveAttempt(
        client,
        options.queryKey,
        async () => pendingAttempt,
      ),
    ).resolves.toEqual(pendingAttempt);

    expect(client.getQueryData(options.queryKey)).toEqual(pendingAttempt);
  });
});
