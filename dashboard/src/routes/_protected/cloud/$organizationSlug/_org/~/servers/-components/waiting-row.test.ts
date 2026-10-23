import { expect, it } from "vitest";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { portText, statusCell } from "./waiting-row";

it("says why a port is unknown instead of a bare Unknown", () => {
  expect(portText({ state: "unknown", reason: "no_answer" })).toBe("No answer · firewall dropping it?");
  expect(portText({ state: "unknown", reason: "port_busy" })).toBe("Couldn't listen · port in use");
  expect(portText({ state: "open" })).toBe("Open");
  expect(portText({ state: "closed" })).toBe("Closed");
});

it("names a host's own blocker over the Docker install it stopped, and asks for a rerun only without one", () => {
  const server = (blockers: WaitingServerView["report"]["blockers"]): Pick<WaitingServerView, "report" | "status" | "lastCheckInAt"> => ({
    status: "blocked",
    lastCheckInAt: new Date(0).toISOString(),
    report: {
      hostname: "MY-SERVER", os: { id: "debian", version: "13" }, cpuCount: 2, memoryTotalBytes: 1024 ** 3,
      prep: { docker: "failed", zfs: "off" }, blockers, disks: [], zfs: { kind: "ineligible", code: "no_disk", message: "No empty disk." },
    },
  });
  const disk = { code: "disk_too_small", message: "Disk is 24 GiB. Ployz needs 30 GiB." };
  const docker = { code: "docker_install_failed", message: "apt-get exited 100." };
  expect(statusCell(server([disk, docker]), 0)).toEqual({ bold: "Disk too small", detail: disk.message });
  expect(statusCell(server([docker]), 0)).toEqual({ bold: "Docker install failed", detail: docker.message });
  expect(statusCell(server([]), 0)).toEqual({ bold: "Docker install failed", detail: "Run the install line again on the server" });
  expect(statusCell({ ...server([disk]), status: "offline" }, 0).bold).toBe("Offline");
});
