// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ServerListItem } from "#/modules/machines/use-servers";
import type { ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { FleetHealth, fleetHealth } from "./fleet-health";

const server = (id: string, status: ServerListItem["status"]) => ({ machine: { id }, name: id, status }) as unknown as ServerListItem;

describe("fleetHealth", () => {
  it("counts each status and the servers behind their release", () => {
    const servers = [server("a", "online"), server("b", "online"), server("c", "offline"), server("d", "building")];
    const upgrades = new Map<string, ServerUpgradeLine>([
      ["a", { kind: "behind", release: "0.3.1", canUpgrade: true } as ServerUpgradeLine],
      ["c", { kind: "when-back", release: "0.3.1" }],
    ]);
    expect(fleetHealth(servers, upgrades)).toEqual({ online: 2, building: 1, offline: 1, unknown: 0, behind: 2 });
  });

  it("hides the states no server is in", () => {
    render(<FleetHealth servers={[server("a", "online")]} upgrades={new Map()} />);
    expect(screen.getByRole("list", { name: "Fleet health" }).textContent).toBe("1 online");
  });
});
