import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { expect, it } from "vitest";
import { dropHiddenRefusals, environmentsQuery, storeViewPrefix } from "./store-view.queries";

const key = (organizationSlug: string, project: string) =>
  [...storeViewPrefix(organizationSlug), "session", "user", environmentsQuery(project)] as const;
const notFound = { ok: false, refusal: { code: "not_found", message: "No Project named shop", details: {} } };

it("drops the hidden views that answered a refusal, and keeps the rest", () => {
  const client = new QueryClient();
  client.setQueryData(key("acme", "shop"), notFound);
  client.setQueryData(key("acme", "store"), { ok: true, value: { environments: [] } });
  client.setQueryData(key("acme", "blog"), notFound);
  client.setQueryData(key("other", "shop"), notFound);
  // Shown: its refusal is what the page shows now, and the page rereads it itself.
  const shown = new QueryObserver(client, { queryKey: key("acme", "blog"), enabled: false });
  const unsubscribe = shown.subscribe(() => {});

  dropHiddenRefusals(client, "acme");

  expect(client.getQueryData(key("acme", "shop"))).toBeUndefined();
  expect(client.getQueryData(key("acme", "store"))).toEqual({ ok: true, value: { environments: [] } });
  expect(client.getQueryData(key("acme", "blog"))).toEqual(notFound);
  expect(client.getQueryData(key("other", "shop"))).toEqual(notFound);
  unsubscribe();
});
