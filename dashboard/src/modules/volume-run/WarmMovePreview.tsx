import { createContext, use, useState, type ReactNode } from "react";
import { useLocation } from "@tanstack/react-router";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { VolumeCopies } from "./volume-copies";
import type { VolumeRunView } from "./volume-run";
import type { VolumeRunRequest } from "./volume-run.functions";

const SCENARIOS = ["Writer only", "Mirror ready", "Move interrupted", "Writer unavailable", "Old copy returned", "Conflicting writers", "Servers unavailable"] as const;
type Scenario = typeof SCENARIOS[number];
type Preview = { scenario: Scenario; setScenario: (scenario: Scenario) => void; active: VolumeRunRequest | null; step: number; start: (run: VolumeRunRequest) => void; advance: () => void; history: readonly VolumeRunRequest[] };
const PreviewContext = createContext<Preview | null>(null);
export const useWarmMovePreview = () => use(PreviewContext);

export function WarmMovePreviewProvider({ children }: { children: ReactNode }) {
  const location = useLocation();
  const [scenario, setScenario] = useState<Scenario>("Mirror ready");
  const [active, setActive] = useState<VolumeRunRequest | null>(null);
  const [step, setStep] = useState(0);
  const [history, setHistory] = useState<VolumeRunRequest[]>([]);
  const enabled = (import.meta.env.DEV || import.meta.env["VITE_WARM_MOVE_PREVIEW"] === "1") && location.hash === "warm-move-preview";
  const preview: Preview = { scenario, setScenario: (next) => { setScenario(next); setActive(null); setStep(0); setHistory([]); }, active, step,
    start: (run) => { setActive(run); setStep(0); }, history,
    advance: () => {
      if (!active) return;
      if (step < 2) { setStep(step + 1); return; }
      setHistory([active, ...history]);
      setScenario(active.kind === "release" ? "Mirror ready" : active.kind === "restore" ? "Writer only" : "Mirror ready");
      setActive(null);
    } };
  return <PreviewContext value={enabled ? preview : null}>{children}</PreviewContext>;
}

export function WarmMovePreviewControls() {
  const preview = useWarmMovePreview();
  if (!preview) return null;
  return <div className="flex flex-col gap-3">
    <div className="flex items-center gap-2"><Badge variant="info">Design preview</Badge><span className="text-xs text-muted-foreground">Sample servers. No real operations.</span></div>
    <Select value={preview.scenario} onValueChange={(value) => { const scenario = SCENARIOS.find((entry) => entry === value); if (scenario) preview.setScenario(scenario); }}>
      <SelectTrigger aria-label="Preview scenario"><SelectValue /></SelectTrigger>
      <SelectContent><SelectGroup>{SCENARIOS.map((scenario) => <SelectItem key={scenario} value={scenario}>{scenario}</SelectItem>)}</SelectGroup></SelectContent>
    </Select>
    {preview.active ? <Button variant="outline" onClick={preview.advance}>{preview.step < 2 ? "Advance preview" : "Complete preview"}</Button> : null}
  </div>;
}

export function previewCopies(preview: Preview, name: string) {
  const runView = (run: VolumeRunRequest, state: "running" | "done", index: number): VolumeRunView => ({ id: `preview-${index}`, volume_id: "preview", volume_name: name, kind: run.kind, args: run.args, orphan: false, state, lease: null, message: null, created_at: "2026-10-10T02:50:00Z", updated_at: "2026-10-10T02:50:00Z", finished_at: state === "done" ? "2026-10-10T02:51:00Z" : null });
  const runs = preview.history.map((run, index) => runView(run, "done", index));
  const active = preview.active ? runView(preview.active, "running", -1) : null;
  if (active) runs.unshift(active);
  const unavailable = preview.scenario === "Writer unavailable";
  const writerOnly = preview.scenario === "Writer only";
  const sealing = preview.scenario === "Old copy returned";
  const twoWriters = preview.scenario === "Conflicting writers";
  const interrupted = preview.scenario === "Move interrupted";
  const unanswered = preview.scenario === "Servers unavailable";
  const blocked = active !== null || twoWriters || unanswered;
  const lastSwitch = preview.history.find((run) => run.kind === "move" || run.kind === "restore");
  const writer = lastSwitch?.kind === "move" ? lastSwitch.args.to : lastSwitch?.kind === "restore" ? lastSwitch.args.from : "web-1";
  const mirror = writer === "web-1" ? "web-2" : "web-1";
  const offer = { servers: [mirror, "web-3"].filter((server) => server !== writer) };
  const view: VolumeCopies = {
    copies: [
      ...(!unavailable ? [{ server: writer, machineId: writer, label: name, role: interrupted || (preview.active?.kind === "move" && preview.step > 0) ? "moving" as const : "writer" as const }] : []),
      ...(!writerOnly ? [{ server: mirror, machineId: mirror, label: `${name}-${mirror}`, role: twoWriters ? "writer" as const : "mirror" as const }] : []),
      ...(sealing ? [{ server: "web-3", machineId: "web-3", label: `${name}-web-3`, role: "old" as const }] : []),
    ], unanswered: unanswered ? ["web-1", "web-2"] : [], active, twoWriters, sealing: sealing ? `${name}-web-3` : null,
    offers: { mirror: !blocked && writerOnly ? offer : null, sync: !blocked && !writerOnly && !unavailable && !interrupted ? { servers: [] } : null,
      move: !blocked && !unavailable && !sealing ? offer : null, release: !blocked && interrupted ? { servers: [] } : null, restore: !blocked && unavailable ? { servers: [mirror] } : null },
  };
  if (interrupted) runs.push({ ...runView({ kind: "move", args: { to: mirror } }, "done", 99), state: "failed", message: "Destination stopped responding during the switch. The original server is paused." });
  const steps = {
    mirror: ["Preparing the destination.", "Copying data. Your service keeps running.", "Confirming the read-only mirror."],
    sync: ["Checking the copies.", "Sending changes to the mirror.", "Confirming the sync."],
    move: ["Copying changes. Your service is still running.", "Switching servers. Your service is briefly stopped.", "Starting the service on the destination."],
    release: ["Checking the interrupted move.", "Resuming the original writer.", "Starting the service on the original server."],
    restore: ["Checking the surviving mirror.", "Making the mirror the writer.", "Confirming the writer. Deploy next to start the service."],
  } satisfies Record<VolumeRunRequest["kind"], readonly string[]>;
  return { view, runs, progress: preview.active ? steps[preview.active.kind][preview.step] ?? null : null };
}
