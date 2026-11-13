// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ActivationReport, ActivationServer } from "#/modules/server-upgrade/activation";
import { ActivationCard } from "./activation-card";

vi.mock("#/modules/organization/members.queries", () => ({
  useRoleGate: () => ({ allowed: true, reason: undefined }),
}));
vi.mock("#/modules/server-upgrade/activation.functions", () => ({
  activateServersServerFn: vi.fn(),
  generateRecipientServerFn: vi.fn(),
  installRecipientServerFn: vi.fn(),
}));

const report = (state: ActivationReport["state"]): ActivationReport => ({
  state,
  operation: state === "awaiting" ? null : "a".repeat(32),
  legacyMembers: [],
  legacyKeys: [],
  privateAccess: null,
});

const server = (digit: string, extra: Partial<ActivationServer>): ActivationServer => ({
  id: digit.repeat(32),
  name: `web-${digit}`,
  answered: true,
  enforces: true,
  member: { id: digit.repeat(32), public_key: Array.from({ length: 32 }, () => Number(digit)), subnet: `10.210.${digit}.0/24` },
  identity: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
  recipient: null,
  activation: report("awaiting"),
  ...extra,
});

const show = (servers: readonly ActivationServer[]) =>
  render(
    <QueryClientProvider client={new QueryClient()}>
      <ActivationCard organizationSlug="ada" servers={servers} />
    </QueryClientProvider>,
  );

afterEach(cleanup);

it("says enforcement is on once every Server activated and holds the new key", () => {
  const recipient = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
  show([
    server("1", { activation: report("activated"), recipient }),
    server("2", { activation: report("activated"), recipient }),
  ]);
  expect(screen.getByText(/Device enforcement is on\./)).toBeTruthy();
  expect(screen.queryByRole("button")).toBeNull();
});

it("offers activation while every Server awaits it, and says nothing about enforcement being on", () => {
  show([server("1", {}), server("2", {})]);
  expect(screen.getByRole("button", { name: "Activate" })).toBeTruthy();
  expect(screen.queryByText(/Device enforcement is on/)).toBeNull();
});
