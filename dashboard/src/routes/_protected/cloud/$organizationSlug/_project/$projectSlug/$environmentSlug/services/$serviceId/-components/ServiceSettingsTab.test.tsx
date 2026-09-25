// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { createContext, use } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Tabs } from "#/components/ui/tabs";
import { asTestDouble } from "#/lib/test-double";
import { RuntimeProvider } from "#/providers/runtime-provider";
import {
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  type ServiceSource,
} from "#/modules/environment-design/services";
import { ServiceSettingsTab } from "./ServiceSettingsTab";
import type { ServiceDrawerState } from "./useServiceDrawerState";

const clients: QueryClient[] = [];
beforeEach(() => {
  vi.stubGlobal("scrollTo", () => {});
  vi.stubGlobal("EventSource", class {
    addEventListener() {}
    removeEventListener() {}
    close() {}
  });
});
afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.unstubAllGlobals();
});

async function show(source: ServiceSource, buildMethod: "dockerfile" | "railpack" = "dockerfile") {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  const update = vi.fn((_id: string, _apply: (draft: ServiceDrawerState["service"]) => void) => ({ isPersisted: { promise: Promise.resolve() } }));
  const build = { buildMethod, dockerfilePath: "docker/Dockerfile", command: null, } as const;
  const state = asTestDouble<ServiceDrawerState>()({
    organizationSlug: "acme",
    environmentSlug: "production",
    service: {
      id: "service", environmentId: "environment", name: "api", privateDns: "api",
      source, build, policy: { autoDeploy: true, waitForCi: false, watchPaths: ["src/**"], imageUpdate: { type: "off" } },
      routes: [], managedHostnames: [], replicas: 1,
      preDeployCommand: null, startCommand: null, healthcheck: { type: "none" },
      restartPolicy: "on-failure", maxRetries: 10,
      registryCredentialUsername: null,
    },
    diff: { field: () => ({ changed: false, baselineValue: undefined }) },
    collection: { update },
    editMetadata: update,
    managedPrefixesInUse: [],
    defaultTargetPort: 8080,
  });
  const State = createContext(state);
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "test-session" }, user: { id: "test-user" } } }) });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug" });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project" });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    component: () => <RuntimeProvider organizationSlug="acme"><Tabs value="settings"><ServiceSettingsTab state={use(State)} /></Tabs></RuntimeProvider>,
  });
  const service = createRoute({ getParentRoute: () => environment, path: "services/$serviceId" });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([service])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production/services/service"] }),
  });
  await router.load();
  const page = (nextSource: ServiceSource) => (
    <QueryClientProvider client={client}>
        <State value={{ ...state, service: { ...state.service, source: nextSource } }}>
          <RouterProvider router={router} />
        </State>
    </QueryClientProvider>
  );
  const view = render(page(source));
  await screen.findByRole("heading", { name: "Source" });
  return { state, update, changeSource: (nextSource: ServiceSource) => view.rerender(page(nextSource)) };
}

it.each([
  createEmptyServiceSource(),
  createImageServiceSource({ image: "nginx:alpine" }),
])("does not offer build controls for a $type source", async (source) => {
  const { update } = await show(source);
  expect(screen.getByRole("heading", { name: "Source" })).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Build" })).toBeNull();
  expect(screen.queryByLabelText("Dockerfile path")).toBeNull();
  expect(screen.queryByLabelText("New watch path")).toBeNull();
  expect(update).not.toHaveBeenCalled();
});

it("restores saved build controls when changing to Git, including a disconnected branch", async () => {
  const image = createImageServiceSource({ image: "nginx:alpine" });
  const { state, update, changeSource } = await show(image);
  const savedBuild = structuredClone(state.service.build);
  const git = createGitServiceSource({ repository: "acme/api", repositoryId: 1, access: { type: "github-installation", installationId: 2  }});
  changeSource(git);
  expect(screen.getByRole("heading", { name: "Build" })).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>("Dockerfile path").value).toBe("docker/Dockerfile");
  expect(screen.getByText("src/**")).toBeTruthy();
  changeSource(image);
  expect(screen.queryByRole("heading", { name: "Build" })).toBeNull();
  changeSource(createGitServiceSource({
    repository: "acme/api", repositoryId: 1, access: { type: "github-installation", installationId: 2 },
    branch: { type: "disconnected", previousName: "main" },
  }));
  expect(screen.getByLabelText<HTMLInputElement>("Dockerfile path").value).toBe("docker/Dockerfile");
  expect(screen.getByText("src/**")).toBeTruthy();
  expect(state.service.build).toEqual(savedBuild);
  expect(update).not.toHaveBeenCalled();
});

it("saves and clears the Railpack build command using the command control", async () => {
  const source = createGitServiceSource({ repository: "acme/api", repositoryId: 1, access: { type: "public" } });
  const { state, update, changeSource } = await show(source, "railpack");
  update.mockImplementation((_id, apply) => {
    apply(state.service);
    return { isPersisted: { promise: Promise.resolve() } };
  });
  fireEvent.click(screen.getByRole("button", { name: "Build command" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Build command" }), { target: { value: "cd dashboard && pnpm build" } });
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
  await waitFor(() => expect(state.service.build.command).toBe("cd dashboard && pnpm build"));
  changeSource(source);
  fireEvent.change(screen.getByRole("textbox", { name: "Build command" }), { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
  await waitFor(() => expect(state.service.build.command).toBeNull());
});
