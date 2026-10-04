// @vitest-environment jsdom
import type { DrainReport, MachineRef, ServiceDrain } from "@ployz/sdk";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LatestDrain } from "#/modules/machines/server-drain";
import { drainView, removeHint, type DrainView } from "#/modules/machines/server-drain-view";
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
const rows = (input: { view: DrainView; acceptsServices?: boolean; unavailable?: string | null }) => {
  const onDrain = vi.fn();
  const onAcceptsServices = vi.fn();
  render(
    <ServerServicesRows
      acceptsServices={input.acceptsServices ?? true}
      onAcceptsServices={onAcceptsServices}
      view={input.view}
      onDrain={onDrain}
      unavailable={input.unavailable ?? null}
    />,
  );
  return { onDrain, onAcceptsServices };
};
const resultRow = (key: string) => document.querySelector(`[data-row="${key}"]`) as HTMLElement;

const moved: ServiceDrain = { service: "shop/api", result: "moved", moves: [{ from: web2, to: web1 }] };
const volume: ServiceDrain = { service: "shop/postgres", result: "stays", reason: { kind: "volume", volume: "pg-data", server: web2 } };
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
    expect(screen.getByText("Move everything running here to your other servers.")).toBeTruthy();
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

  it("shows the summary and each Service's outcome, and offers Drain again", () => {
    rows({ view: view(finished([moved, volume, retired, failed], null, false)) });
    expect(screen.getByText(/1 moved, 1 stopped, 1 stayed, 1 failed/)).toBeTruthy();
    expect(within(resultRow("shop/api")).getByText("Moved to web-1")).toBeTruthy();
    expect(within(resultRow("shop/postgres")).getByText("Stayed")).toBeTruthy();
    expect(within(resultRow("shop/postgres")).getByText("Its volume pg-data is on this server")).toBeTruthy();
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
  const LEAD = "Services turn off for web-2, and what runs here moves to your other servers one at a time.";
  const NOTHING_BACK = "Turning services back on doesn’t move anything back.";
  const inOrder = (first: HTMLElement, ...rest: HTMLElement[]) => {
    let previous = first;
    for (const element of rest) {
      if (!(previous.compareDocumentPosition(element) & Node.DOCUMENT_POSITION_FOLLOWING)) return false;
      previous = element;
    }
    return true;
  };

  it("leads with what Drain does, lists what runs here, then says what stays, and drains on confirm", () => {
    const onConfirm = vi.fn();
    render(<DrainDialog serverName="web-2" names={["api", "postgres"]} unowned={[]} open onOpenChange={() => {}} onConfirm={onConfirm} />);
    expect(screen.getByText("Drain web-2?")).toBeTruthy();
    const list = screen.getByLabelText("Running on web-2");
    expect(within(list).getByText("api")).toBeTruthy();
    expect(within(list).getByText("postgres")).toBeTruthy();
    expect(inOrder(
      screen.getByText(LEAD),
      list,
      screen.getByText("Services that use a volume here stay."),
      screen.getByText(NOTHING_BACK),
    )).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Drain" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("says so when nothing runs here", () => {
    render(<DrainDialog serverName="web-2" names={[]} unowned={[]} open onOpenChange={() => {}} onConfirm={() => {}} />);
    expect(inOrder(screen.getByText(LEAD), screen.getByText("Nothing runs here now."), screen.getByText(NOTHING_BACK))).toBe(true);
    expect(screen.queryByLabelText("Running on web-2")).toBeNull();
  });

  it("names what stays because no Project owns it, and never says nothing runs here", () => {
    render(<DrainDialog serverName="web-2" names={[]} unowned={["old"]} open onOpenChange={() => {}} onConfirm={() => {}} />);
    expect(screen.getByText("Nothing here for Drain to move.")).toBeTruthy();
    expect(screen.getByText("old stays: no project owns it. Services that use a volume here stay.")).toBeTruthy();
    expect(screen.queryByText("Nothing runs here now.")).toBeNull();
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
