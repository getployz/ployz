// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import { useOpenCreatedService, type CreatedService } from "./useServiceCreator";

afterEach(cleanup);

const params = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" };
const CANVAS = "/cloud/acme/shop/production";

function Create({ created }: { created: CreatedService }) {
  const openCreated = useOpenCreatedService(params);
  return <button onClick={() => void openCreated(created)}>Create</button>;
}

async function openCanvas(created: CreatedService, at = CANVAS) {
  const root = createRootRoute({ component: Outlet });
  const environment = createRoute({
    getParentRoute: () => root,
    path: "cloud/$organizationSlug/$projectSlug/$environmentSlug",
    component: () => <><Create created={created} /><Outlet /></>,
  });
  const index = createRoute({ getParentRoute: () => environment, path: "/" });
  const service = createRoute({ getParentRoute: () => environment, path: "services/$serviceId" });
  const elsewhere = createRoute({ getParentRoute: () => root, path: "elsewhere", component: () => <p>Elsewhere page</p> });
  const router = createRouter({
    routeTree: root.addChildren([environment.addChildren([index, service]), elsewhere]),
    history: createMemoryHistory({ initialEntries: ["/elsewhere", at] }),
  });
  render(<RouterProvider router={router} />);
  await screen.findByRole("button", { name: "Create" });
  return router;
}

function pendingWrite() {
  let refuse!: () => void;
  let save!: () => void;
  const persisted = new Promise<void>((resolve, reject) => { save = resolve; refuse = () => reject(new Error("refused")); });
  persisted.catch(() => {});
  return { created: { service: { id: "new" }, persisted }, refuse, save };
}

it("opens the new service before its write lands, and keeps it open once saved", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  write.save();
  await write.created.persisted;
  expect(router.history.location.pathname).toBe(`${CANVAS}/services/new`);
});

it("returns to the canvas when the Store refuses the new service", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  write.refuse();
  await waitFor(() => expect(router.history.location.state.__TSR_index).toBe(1));
  expect(router.state.location.pathname).toBe(CANVAS);
  router.history.forward();
  await waitFor(() => expect(router.history.location.state.__TSR_index).toBe(2));
  expect(router.state.location.pathname).toBe(CANVAS);
  router.history.go(-2);
  await waitFor(() => expect(router.state.location.pathname).toBe("/elsewhere"));
});

it("steps back to the canvas when the panel's entry was replaced, as a tab change does", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  router.history.replace(`${CANVAS}/services/new?tab=variables`);
  await waitFor(() => expect(router.state.location.searchStr).toBe("?tab=variables"));
  write.refuse();
  await waitFor(() => expect(router.history.location.state.__TSR_index).toBe(1));
  expect(router.state.location.pathname).toBe(CANVAS);
  router.history.back();
  await waitFor(() => expect(router.state.location.pathname).toBe("/elsewhere"));
});

it("goes to the canvas without stepping back when the panel opened from another panel", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created, `${CANVAS}/services/api`);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  write.refuse();
  await waitFor(() => expect(router.state.location.pathname).toBe(CANVAS));
  expect(router.history.location.state.__TSR_index).toBe(2);
  router.history.back();
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/api`));
});

it("returns to the canvas when the Store refuses a service whose panel moved on to another entry", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  router.history.push(`${CANVAS}/services/new?tab=variables`);
  await waitFor(() => expect(router.state.location.searchStr).toBe("?tab=variables"));
  write.refuse();
  await waitFor(() => expect(router.state.location.pathname).toBe(CANVAS));
});

it("leaves someone who moved on where they are when the Store refuses", async () => {
  const write = pendingWrite();
  const router = await openCanvas(write.created);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`${CANVAS}/services/new`));
  router.history.push("/elsewhere");
  await screen.findByText("Elsewhere page");
  write.refuse();
  await write.created.persisted.catch(() => {});
  expect(router.history.location.pathname).toBe("/elsewhere");
});
