// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, renderHook } from "@testing-library/react";
import { CipherSuite } from "@hpke/core";
import { Result } from "effect";
import type { EnvironmentView } from "@ployz/sdk";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as sealingFunctions from "#/modules/sealing/sealing.functions";
import * as seal from "#/modules/variables/seal.client";
import { useSealing } from "#/modules/variables/sealing";
import { diffVariables, parseEnv, parseJson } from "#/modules/variables/variable-raw-editor";
import * as functions from "./store.functions";
import { serviceSettingRows } from "./store-services";
import { serviceVariables, storeVariableWriter } from "./store-variables";
import { environmentSettingsQuery, storeViewOptions, useStoreView } from "./store-view.queries";
import { useStoreWriter } from "./store-write";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const environment = { project: "shop", environment: "production" };
const view: EnvironmentView = {
  environment: { id: "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f", project: "shop", name: "production", revision: 3 },
  settings: [
    { path: "web.env.API_KEY", value: { secret: false }, default: null, apply: "staged" },
    { path: "web.env.API_KEY.exported", value: false, default: false, apply: "staged" },
    { path: "web.env.LOG_LEVEL", value: "info", default: null, apply: "staged" },
    { path: "web.env.LOG_LEVEL.exported", value: true, default: false, apply: "staged" },
    { path: "web.env.OLD", value: "x", default: null, apply: "staged" },
  ],
};

/** What each write handed the server function at 686f82827, before the browser sealed anything. */
const TRUNK = {
  sealedAdd: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.TOKEN\",\"value\":{\"secret\":\"s3cret-value\"}},{\"op\":\"set\",\"path\":\"web.env.TOKEN.exported\",\"value\":false}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
  overwrite: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.LOG_LEVEL\",\"value\":{\"secret\":\"overwritten\"}},{\"op\":\"set\",\"path\":\"web.env.LOG_LEVEL.exported\",\"value\":true}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
  menuSeal: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.LOG_LEVEL\",\"value\":{\"secret\":\"info-sealed\"}}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
  setValue: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.API_KEY\",\"value\":{\"secret\":\"now-it-has-one\"}}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
  rawEnv: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.NEW_ONE\",\"value\":\"hello\"},{\"op\":\"set\",\"path\":\"web.env.LOG_LEVEL\",\"value\":\"debug\"},{\"op\":\"unset\",\"path\":\"web.env.OLD\"}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
  rawJson: "{\"organizationSlug\":\"acme\",\"command\":{\"command\":\"edit\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"expect\":3,\"changes\":[{\"op\":\"set\",\"path\":\"web.env.FROM_JSON\",\"value\":\"yes\"},{\"op\":\"set\",\"path\":\"web.env.LOG_LEVEL\",\"value\":\"warn\"},{\"op\":\"unset\",\"path\":\"web.env.OLD\"}]},\"shows\":[{\"query\":\"environment\",\"environment\":{\"project\":\"shop\",\"environment\":\"production\"},\"path\":null,\"all\":true}]}",
};

it("sends trunk's exact write for every Sealed and raw save while sealing is off, sealing nothing", async () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(async () => ({ ok: true, value: { view: "environment", ...view } }) as never);
  const read = vi.spyOn(sealingFunctions, "readSealingFn").mockResolvedValue({ organizationId: "org", sealing: { state: "off" } });
  const sealValue = vi.spyOn(seal, "sealValue");
  const hpke = vi.spyOn(CipherSuite.prototype, "seal");
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue(
    { ok: true, value: { written: "edited", environment: view.environment, staged: [], immediate: [], typed_addresses: [] } } as never);
  const query = environmentSettingsQuery(environment);
  queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value: { view: "environment", ...view } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  renderHook(() => useStoreView("acme", query), { wrapper });
  const { prepare } = renderHook(() => useSealing("acme"), { wrapper }).result.current;
  await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(1));
  const settings = serviceSettingRows(view, "web");
  const variables = serviceVariables(settings, "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6");
  const writer = storeVariableWriter(renderHook(() => useStoreWriter("acme"), { wrapper }).result.current, environment, "web", variables, settings,
    { prepare, target: { environment: view.environment.id, service: "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6" } });
  const raw = (parsed: ReturnType<typeof parseEnv>) => {
    if (Result.isFailure(parsed)) throw parsed.failure;
    const { creates, updates, deletes } = diffVariables(parsed.success, variables);
    return writer.replace([...creates, ...updates], deletes);
  };
  const saves = {
    sealedAdd: () => writer.create("TOKEN", "s3cret-value", true, false),
    overwrite: () => writer.create("LOG_LEVEL", "overwritten", true, true),
    menuSeal: () => writer.seal("LOG_LEVEL", "info-sealed"),
    setValue: () => writer.seal("API_KEY", "now-it-has-one"),
    rawEnv: () => raw(parseEnv("LOG_LEVEL=debug\nNEW_ONE=hello\n")),
    rawJson: () => raw(parseJson('{"LOG_LEVEL":"warn","FROM_JSON":"yes"}')),
  };
  const reads: Record<string, number> = {};
  for (const [name, save] of Object.entries(saves)) {
    write.mockClear();
    read.mockClear();
    await save().isPersisted.promise;
    expect(write.mock.calls.map(([call]) => JSON.stringify(call.data)), name).toEqual([TRUNK[name as keyof typeof TRUNK]]);
    reads[name] = read.mock.calls.length;
  }
  // A Sealed save reads afresh whether its Organization seals; a plain one reads nothing.
  expect(reads).toEqual({ sealedAdd: 1, overwrite: 1, menuSeal: 1, setValue: 1, rawEnv: 0, rawJson: 0 });
  expect(sealValue).not.toHaveBeenCalled();
  expect(hpke).not.toHaveBeenCalled();
});
