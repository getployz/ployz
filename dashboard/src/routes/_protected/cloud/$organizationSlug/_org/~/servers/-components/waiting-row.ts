import type { PortState } from "#/modules/machines/waiting/probe";
import { ZFS_BUILD_FAILED } from "#/modules/machines/waiting/server-enrollment";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";

export type Readiness = "ready" | "preparing" | "blocked" | "offline";

export const READINESS_FILTERS = [
  { value: "all", label: "All" },
  { value: "ready", label: "Ready" },
  { value: "preparing", label: "Preparing" },
  { value: "blocked", label: "Blocked" },
  { value: "offline", label: "Offline" },
] as const;
export type ReadinessFilter = (typeof READINESS_FILTERS)[number]["value"];

const BLOCKER_LABELS = new Map(Object.entries({
  disk_too_small: "Disk too small",
  no_systemd: "No systemd",
  docker_install_failed: "Docker install failed",
  docker_stopped: "Docker stopped",
  docker_too_old: "Docker too old",
  docker_unreadable: "Can't read Docker",
  buildx_too_old: "Buildx too old",
  docker_containerd_store_off: "Image store off",
  unsupported_os: "Unsupported OS",
  secure_boot: "Secure Boot on",
  secure_boot_unknown: "Secure Boot unknown",
  openvz: "OpenVZ",
  unprivileged_lxc: "Unprivileged LXC",
  lxc_privilege_unknown: "LXC unknown",
}));

export function blockerLabel(code: string) {
  return BLOCKER_LABELS.get(code) ?? "Blocked";
}

export function readiness(server: Pick<WaitingServerView, "report" | "status" | "lastCheckInAt">): Readiness {
  switch (server.status) {
    case "offline":
      return "offline";
    case "blocked":
      return "blocked";
    case "preparing":
      return server.report.prep?.docker === "failed" ? "blocked" : "preparing";
    case "waiting":
      return "ready";
  }
}

export function selectable(server: Pick<WaitingServerView, "report" | "status" | "lastCheckInAt">) {
  const state = readiness(server);
  return state === "ready" || state === "preparing";
}

function ago(iso: string, now: number) {
  const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  return hours < 48 ? `${hours}h ago` : `${Math.round(hours / 24)}d ago`;
}

export function lastSeen(server: Pick<WaitingServerView, "report" | "status" | "lastCheckInAt">, now: number) {
  return server.status === "offline" ? `Last seen ${ago(server.lastCheckInAt, now)}` : `Seen ${ago(server.lastCheckInAt, now)}`;
}

export type TwoLines = { readonly bold: string; readonly detail: string };

export function statusCell(server: Pick<WaitingServerView, "report" | "status" | "lastCheckInAt">, now: number): TwoLines {
  const { report } = server;
  const seen = ago(server.lastCheckInAt, now);
  if (server.status === "offline") return { bold: "Offline", detail: seen };
  if (report.prep?.docker === "failed") {
    const failure = report.blockers.find((blocker) => blocker.code === "docker_install_failed");
    return { bold: "Docker install failed", detail: failure?.message ?? "Run the install line again on the server" };
  }
  const blocker = report.blockers[0];
  if (blocker !== undefined) return { bold: blockerLabel(blocker.code), detail: blocker.message };
  if (report.prep?.docker === "installing") return { bold: "Installing Docker", detail: seen };
  if (report.prep?.zfs === "installing") return { bold: "Building ZFS", detail: seen };
  if (report.prep?.zfs === "failed") return { bold: "Ready", detail: `${ZFS_BUILD_FAILED} · ${seen}` };
  return { bold: "Ready", detail: seen };
}

const GB = 1024 ** 3;

export function bytes(value: number) {
  return value >= 1024 * GB ? `${Math.round((value / (1024 * GB)) * 10) / 10} TB` : `${Math.round(value / GB)} GB`;
}

export function sizeText(report: WaitingServerView["report"]) {
  return `${report.cpuCount} vCPU · ${bytes(report.memoryTotalBytes)}`;
}

export function disksText(report: WaitingServerView["report"]) {
  const root = report.disks.find((disk) => disk.role === "root");
  const empty = report.disks.filter((disk) => disk.role === "empty");
  return { main: root === undefined ? "No disk" : bytes(root.sizeBytes), spare: empty.map((disk) => `+${bytes(disk.sizeBytes)}`).join(" ") };
}

export function volumesText(report: WaitingServerView["report"]): TwoLines {
  if (report.zfs.kind === "ineligible") return { bold: "Docker only", detail: report.zfs.message };
  const minutes = report.zfs.buildMinutes;
  const bold = minutes === undefined ? "ZFS" : `ZFS · ~${Math.max(1, Math.round(minutes))} min`;
  return { bold, detail: "Main disk" };
}

export function providerOf(server: Pick<WaitingServerView, "report">) {
  return server.report.provider ?? "Unknown";
}

export function portText(port: PortState): string {
  if (port.state !== "unknown") return port.state === "open" ? "Open" : "Closed";
  switch (port.reason) {
    case "pending":
      return "Checking…";
    case "private_range":
      return "Not checked · private address";
    case "no_address":
      return "Not checked · no address";
    case "address_unstable":
      return "Not checked · address keeps changing";
    case "no_answer":
      return "No answer · firewall dropping it?";
    case "other_listener":
      return "Something else answered";
    case "port_busy":
      return "Couldn't listen · port in use";
    case "no_host_report":
      return "Unknown · server didn't report";
  }
}

export function hostnameOrIpMatcher(query: string): (server: Pick<WaitingServerView, "report" | "observedIp">) => boolean {
  const term = query.trim().toLowerCase();
  if (term === "") return () => true;
  const test = term.includes("*")
    ? ((pattern: RegExp) => (value: string) => pattern.test(value))(
      new RegExp(`^${term.split("*").map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`))
    : (value: string) => value.includes(term);
  return (server) => test(server.report.hostname.toLowerCase()) || test(server.observedIp);
}
