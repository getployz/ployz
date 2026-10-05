// @vitest-environment jsdom
import type { DrainReport, MachineRef, ServiceDrain } from "@ployz/sdk";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LatestDrain } from "#/modules/machines/server-drain";
import { drainView, removeHint, type DrainDialogRow, type DrainRow, type DrainView } from "#/modules/machines/server-drain-view";
import { DrainDialog } from "./drain-dialog";
import { RemoveServerHint } from "./remove-server-section";
import { DrainButton, ServerServicesRows } from "./server-services-section";
import { runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";

afterEach(cleanup);

const ref = (digit: string, name: string): MachineRef => {
  const { id } = runtimeWatchMachineFixture(digit.repeat(32), name);
  return { id, name };
};
const web1 = ref("1", "web-1");
const web2 = ref("2", "web-2");
const at = new Date().toISOString();

const finished = (services: ServiceDrain[], stopped: DrainReport["stopped"] = null, complete = stopped === null): LatestDrain => ({
  attemptId: "a1",
  state: "finished",
  endedAt: at,
  report: { server: runtimeWatchMachineFixture(web2.id, web2.name), services_role: "turned_off", services, stopped, remaining: { kind: "observed", services: [], unchosen: [] }, complete },
});
const view = (latest: LatestDrain | null, requested: string | null = null) => drainView({
  server: web2,
  latest: latest === null ? {} : { [web2.id]: latest },
  serverNames: new Map([[web1.id, web1.name], [web2.id, web2.name]]),
  requested,
});
const rows = (input: { view: DrainView; left?: DrainRow[]; acceptsServices?: boolean; unavailable?: string | null }) => {
  const onDrain = vi.fn();
  const onAcceptsServices = vi.fn();
  render(
    <ServerServicesRows
      acceptsServices={input.acceptsServices ?? true}
      onAcceptsServices={onAcceptsServices}
      view={input.view}
      left={input.left ?? []}
      onDrain={onDrain}
      unavailable={input.unavailable ?? null}
    />,
  );
  return { onDrain, onAcceptsServices };
};
const resultRow = (key: string) => document.querySelector(`[data-row="${key}"]`) as HTMLElement;

const moved: ServiceDrain = { service: "shop/api", result: "moved", moves: [{ from: web2, to: web1 }] };
const volume: ServiceDrain = { service: "shop/postgres", result: "stays", reason: { kind: "volume", server: web2 } };
const retired: ServiceDrain = { service: "monitoring/node-exporter", result: "retired" };
const failed: ServiceDrain = {
  service: "shop/worker",
  result: "failed",
  moves: [{ from: web2, to: web1 }],
  failure: { stage: "copy_image", from: web2, to: web1, detail: "disk full" },
};

describe("Services section", () => {
  it("says what Drain does and drains on click", () => {
    const { onDrain, onAcceptsServices } = rows({ view: view(null) });
    expect(screen.getByText("Move all services to your other servers.")).toBeTruthy();
    expect(screen.queryByText(/Nothing new starts here/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Drain" }));
    expect(onDrain).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("switch"));
    expect(onAcceptsServices).toHaveBeenCalledWith(false);
  });

  it("explains a Server that takes no new Services", () => {
    rows({ view: view(null), acceptsServices: false });
    expect(screen.getByText("Nothing new starts here. What runs here stays until you drain.")).toBeTruthy();
  });

  it("spins and holds the switch while a Drain is under way", () => {
    rows({ view: view(null, "a2") });
    expect(screen.getByText("Starting…")).toBeTruthy();
    expect((screen.getByRole("button", { name: /Draining…/ }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("switch").getAttribute("aria-disabled")).toBe("true");
  });

  it("lists what a running Drain still has to move, and counts it", () => {
    const left: DrainRow = { key: "shop/postgres", name: "postgres", namespace: "shop", global: false, tone: "neutral", label: "Pending", reason: null, pinned: false };
    rows({ view: view({ attemptId: "a1", state: "running", startedAt: at }), left: [left] });
    expect(screen.getByText(/Draining · 1 left · Started/)).toBeTruthy();
    expect(within(screen.getByLabelText("Left to move")).getByText("postgres")).toBeTruthy();
    expect(screen.queryByLabelText("Drain result")).toBeNull();
    cleanup();
    rows({ view: view({ attemptId: "a1", state: "running", startedAt: at }) });
    expect(screen.getByText(/Draining · Finishing up · Started/)).toBeTruthy();
  });

  it("shows the summary and each Service's outcome, and offers Drain again", () => {
    rows({ view: view(finished([moved, volume, retired, failed], null, false)) });
    expect(screen.getByText(/1 moved, 1 stopped, 1 stayed, 1 failed/)).toBeTruthy();
    expect(within(resultRow("shop/api")).getByText("Moved to web-1")).toBeTruthy();
    expect(within(resultRow("shop/postgres")).getByText("Stayed")).toBeTruthy();
    expect(within(resultRow("shop/postgres")).getByText("Its volume is on this server")).toBeTruthy();
    expect(within(resultRow("monitoring/node-exporter")).getByText("Stopped here")).toBeTruthy();
    expect(within(resultRow("shop/worker")).getByText("Failed")).toBeTruthy();
    expect(within(resultRow("shop/worker")).getByText(/Couldn't copy its image to web-1\. It still runs here\. Moved to web-1 before that\./))
      .toBeTruthy();
    expect(screen.getByRole("button", { name: "Drain again" })).toBeTruthy();
  });

  it("warns when the Drain stopped early", () => {
    rows({ view: view(finished([moved], { kind: "entry_unreachable", detail: "timeout" })) });
    expect(screen.getByText("It stopped early: Lost contact with your servers.")).toBeTruthy();
  });

  it("says why Drain can't run, and won't run", () => {
    const { onDrain } = rows({ view: view(null), unavailable: "Drain needs web-2 online." });
    expect(screen.getByText("Drain needs web-2 online.")).toBeTruthy();
    const button = screen.getByRole("button", { name: "Drain" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(onDrain).not.toHaveBeenCalled();
  });
});

describe("Drain dialog", () => {
  const inOrder = (first: HTMLElement, ...rest: HTMLElement[]) => {
    let previous = first;
    for (const element of rest) {
      if (!(previous.compareDocumentPosition(element) & Node.DOCUMENT_POSITION_FOLLOWING)) return false;
      previous = element;
    }
    return true;
  };

  const LEAD = "Moves every service to your other servers, one at a time. Nothing moves back on its own.";
  const VOLUMES = "Services with a volume on web-2 stay.";
  const row = (name: string, stays: DrainDialogRow["stays"] = null): DrainDialogRow => ({ key: `shop/${name}`, name, namespace: "shop", stays });
  const dialog = (rows: DrainDialogRow[], onConfirm = () => {}) =>
    render(<DrainDialog serverName="web-2" rows={rows} open onOpenChange={() => {}} onConfirm={onConfirm} />);
  const listed = (key: string) => document.querySelector(`[data-row="${key}"]`) as HTMLElement;

  it("leads with what Drain does, lists what runs here, and drains on confirm", () => {
    const onConfirm = vi.fn();
    dialog([row("api"), row("worker")], onConfirm);
    expect(screen.getByText("Drain web-2")).toBeTruthy();
    const list = screen.getByLabelText("Running on web-2");
    expect(within(list).getByText("api")).toBeTruthy();
    expect(inOrder(screen.getByText(LEAD), list, screen.getByText(VOLUMES))).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Drain" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("badges each Service a Drain won't move, with why", () => {
    dialog([row("api"), row("postgres", "data"), row("old", "unowned")]);
    expect(within(listed("shop/api")).queryByText("Stays")).toBeNull();
    expect(within(listed("shop/postgres")).getByText("Stays")).toBeTruthy();
    expect(within(listed("shop/postgres")).getByText("Data on this server")).toBeTruthy();
    expect(within(listed("shop/old")).getByText("Skipped")).toBeTruthy();
    expect(within(listed("shop/old")).getByText("No project owns it")).toBeTruthy();
    expect(screen.queryByText("Nothing to move.")).toBeNull();
  });

  it("says so when nothing runs here", () => {
    dialog([]);
    expect(screen.getByText("Nothing is running on web-2.")).toBeTruthy();
    expect(screen.queryByLabelText("Running on web-2")).toBeNull();
  });

  it("says when nothing here will move", () => {
    dialog([row("old", "unowned")]);
    expect(screen.getByText("Nothing to move.")).toBeTruthy();
  });
});

describe("Remove hint", () => {
  const hint = (latest: LatestDrain | null, running: { identity: string; name: string; namespace: string }[]) => {
    const current = view(latest);
    const onDrain = vi.fn();
    render(<RemoveServerHint hint={removeHint(current, running, new Set(["left-behind"]))} drainButton={<DrainButton view={current} onClick={onDrain} size="sm" />} />);
    return onDrain;
  };

  it("suggests draining first, with the Drain button", () => {
    const onDrain = hint(null, [{ identity: "shop/api", name: "api", namespace: "shop" }, { identity: "shop/web", name: "web", namespace: "shop" }]);
    expect(screen.getByRole("note").textContent).toContain("2 services still run here. Drain first to move them.");
    fireEvent.click(screen.getByRole("button", { name: "Drain" }));
    expect(onDrain).toHaveBeenCalledTimes(1);
  });

  it("says a Service whose volume is here won't move", () => {
    hint(finished([moved, volume], null, false), [{ identity: "shop/postgres", name: "postgres", namespace: "shop" }]);
    expect(screen.getByRole("note").textContent).toBe("postgres still runs here. Its volume is on this server.");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("names what no Project owns on its own line, and offers no Drain for it", () => {
    hint(null, [{ identity: "left-behind/old", name: "old", namespace: "left-behind" }]);
    expect(screen.getByRole("note").textContent).toBe("old still runs here. No project owns it, so Drain leaves it.");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("is quiet when nothing runs here", () => {
    hint(null, []);
    expect(screen.queryByRole("note")).toBeNull();
  });
});
