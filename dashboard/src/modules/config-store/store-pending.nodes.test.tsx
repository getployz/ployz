// @vitest-environment jsdom
import { renderHook } from "@testing-library/react";
import { MutationObserver, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { expect, it } from "vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { storeWriteKey, useStorePendingNodes } from "./store-pending";
import { createConfigCommand } from "./store-configs";
import { createVolumeCommand } from "./store-volumes";

const environment = { project: "shop", environment: "production" };
const staging = { project: "shop", environment: "staging" };

function pendingNodes(commands: readonly ConfigCommand[]) {
  const queryClient = new QueryClient();
  for (const command of commands) {
    void new MutationObserver<unknown, Error, { command: ConfigCommand }>(queryClient, { mutationKey: storeWriteKey("acme", command.command), mutationFn: () => new Promise(() => {}) }).mutate({ command });
  }
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  return renderHook(() => useStorePendingNodes("acme", environment), { wrapper }).result.current;
}

it("draws a pending Volume and Config as arriving by kind, and only in their own Environment", () => {
  const { arriving } = pendingNodes([
    createVolumeCommand("vol-1", environment, "data", { kind: "docker" }),
    createConfigCommand("cfg-1", environment, "nginx"),
    createVolumeCommand("vol-2", staging, "cache", { kind: "docker" }),
  ]);
  expect(arriving).toEqual([
    { pending: "arriving", node: "volume", id: "vol-1", name: "data" },
    { pending: "arriving", node: "config", id: "cfg-1", name: "nginx" },
  ]);
});

it("files a pending removal as leaving under its own kind, so a Volume and a Config sharing a name stay apart", () => {
  const { leaving } = pendingNodes([
    { command: "remove_volume", environment, volume: "data" },
    { command: "delete_config", environment, config: "nginx" },
  ]);
  expect([...leaving.volume]).toEqual(["data"]);
  expect([...leaving.config]).toEqual(["nginx"]);
  expect(leaving.service.size).toBe(0);
});
