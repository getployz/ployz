// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { useLiveQuery } from "@tanstack/react-db";
import { createApiCollection } from "#/collections/query-collection";
import { OrganizationCollectionRefreshNotice } from "./organization-collection-refresh-notice";

afterEach(cleanup);

it("shows active scoped refresh errors while retaining rows, clears on recovery, and ignores inactive or other scopes", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  let fail = false;
  const key = ["collections", "session", "user", "acme", "environment_deployment"];
  const raw = createApiCollection({
    queryClient,
    queryKey: key,
    queryFn: async () => {
      if (fail) throw new Error("offline");
      return [{ id: "deploy", status: "running" }];
    },
    getKey: (row: { id: string; status: string }) => row.id,
  });
  function Rows() {
    const { data } = useLiveQuery(raw);
    return data.map((row) => <p key={row.id}>{row.status}</p>);
  }
  function View({ active = true, sessionId = "session", userId = "user", organizationSlug = "acme" }) {
    return <>
      <OrganizationCollectionRefreshNotice scope={{ ...scope, sessionId, userId }} organizationSlug={organizationSlug} />
      {active && <Rows />}
    </>;
  }
  const view = render(<View />);
  await screen.findByText("running");
  expect(screen.queryByRole("alert")).toBeNull();
  fail = true;
  await act(async () => { await raw.utils.refetch(); });
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("may be out of date"));
  expect(screen.getByText("running")).toBeTruthy();
  for (const otherScope of [{ sessionId: "other" }, { userId: "other" }, { organizationSlug: "other" }]) {
    view.rerender(<View {...otherScope} />);
    expect(screen.queryByRole("alert")).toBeNull();
  }
  view.rerender(<View />);
  expect(screen.getByRole("alert")).toBeTruthy();
  fail = false;
  await act(async () => { await raw.utils.refetch(); });
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  fail = true;
  await act(async () => { await raw.utils.refetch(); });
  await screen.findByRole("alert");
  view.rerender(<View active={false} />);
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  expect(queryClient.getQueryState(key)?.status).toBe("error");
  cleanup();
  await raw.cleanup();
  queryClient.clear();
});
