import { type ReactNode, useState } from "react";
import { MinusIcon, PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldTitle } from "#/components/ui/field";
import { Progress, ProgressLabel, ProgressValue } from "#/components/ui/progress";
import { Switch } from "#/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import {
  type PolicyCount, POLICY_LIMITS, type SnapshotEvery, type SnapshotPolicy, clampCount, defaultKeep, deploysWords, keepSpanWords, pruneIds,
  scheduleWords,
} from "#/modules/volume-copies/snapshot-schedule";
import type { Snapshot } from "#/modules/volume-copies/volume-copies";
import type { useSnapshotPolicy } from "#/modules/volume-copies/volume-snapshots.hooks";
import { sizeWords } from "#/modules/volume-copies/volume-snapshots";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

export type VolumeScheduleData = ReturnType<typeof useSnapshotPolicy>;

/** The policy's words and an Edit that opens the dialog; the Snapshots view header and the Settings tab both show it. */
export function useScheduleEditor(volume: string, schedule: VolumeScheduleData, snapshots: readonly Snapshot[]) {
  const [editing, setEditing] = useState(false);
  const edit = <Button variant="outline" size="sm" disabled={schedule.policy === null} onClick={() => setEditing(true)}>Edit</Button>;
  const dialog = <ScheduleDialog volume={volume} policy={editing ? schedule.policy : null} snapshots={snapshots} pending={schedule.pending}
    onOpenChange={setEditing} onSave={schedule.save} />;
  return { edit, dialog };
}

/** The Settings tab's Snapshots: space used against the cap, the writer, and the policy in words. */
export function SnapshotSettings({ volume, server, schedule, snapshots }: {
  volume: string;
  server: string;
  schedule: VolumeScheduleData;
  snapshots: readonly Snapshot[];
}) {
  const editor = useScheduleEditor(volume, schedule, snapshots);
  const policy = schedule.policy;
  const used = snapshots.reduce((sum, snapshot) => sum + snapshot.bytes, 0);
  return <SettingsSection id="snapshots" title="Snapshots" action={editor.edit}>
    {policy === null ? null : <Progress value={Math.min(100, (used / (policy.capGb * 1e9)) * 100)}>
      <ProgressLabel>Snapshots</ProgressLabel>
      <ProgressValue>{() => `${sizeWords(used)} of ${policy.capGb} GB`}</ProgressValue>
    </Progress>}
    <Line title="Server"><span className="text-sm text-muted-foreground">{server}</span></Line>
    <Line title="Deploys"><span className="text-sm text-muted-foreground">{policy === null ? "" : deploysWords(policy)}</span></Line>
    <Line title="Schedule"><span className="text-sm text-muted-foreground">{policy === null ? "" : scheduleWords(policy)}</span></Line>
    {editor.dialog}
  </SettingsSection>;
}

/** Edits a Volume's automatic snapshots. Save runs now and prunes at once, so the footer says what it deletes. */
export function ScheduleDialog({ volume, policy, snapshots, pending, onOpenChange, onSave }: {
  volume: string;
  policy: SnapshotPolicy | null;
  snapshots: readonly Snapshot[];
  pending: boolean;
  onOpenChange: (open: boolean) => void;
  onSave: (policy: SnapshotPolicy) => Promise<boolean>;
}) {
  return <Dialog open={policy !== null} onOpenChange={onOpenChange}>
    <DialogContent>
      {policy === null ? null
        : <ScheduleForm volume={volume} initial={policy} snapshots={snapshots} pending={pending} onCancel={() => onOpenChange(false)} onSave={onSave} />}
    </DialogContent>
  </Dialog>;
}

function ScheduleForm({ volume, initial, snapshots, pending, onCancel, onSave }: {
  volume: string;
  initial: SnapshotPolicy;
  snapshots: readonly Snapshot[];
  pending: boolean;
  onCancel: () => void;
  onSave: (policy: SnapshotPolicy) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState(initial);
  const deletes = pruneIds(snapshots, draft).length;
  const set = (change: Partial<SnapshotPolicy>) => setDraft((current) => ({ ...current, ...change }));
  const every = (next: SnapshotEvery) => set(next === "off" ? { every: next } : { every: next, keep: defaultKeep(next) });

  return <>
    <DialogHeader><DialogTitle>Snapshots · {volume}</DialogTitle></DialogHeader>
    <div className="flex flex-col gap-3">
      <Section title="Deploys">
        <Line title="Before"><Switch aria-label="Before" checked={draft.before} onCheckedChange={(before) => set({ before })} /></Line>
        <Line title="After"><Switch aria-label="After" checked={draft.after} onCheckedChange={(after) => set({ after })} /></Line>
        <Line title="Keep"><Stepper name="Deploys kept" count="keepDeploys" value={draft.keepDeploys} unit="deploys" onChange={(keepDeploys) => set({ keepDeploys })} /></Line>
      </Section>
      <Section title="Schedule">
        <Line title="Every">
          <ToggleGroup aria-label="Every" variant="outline" size="sm" spacing={0} value={[draft.every]}
            onValueChange={([next]) => { if (next === "hour" || next === "day" || next === "off") every(next); }}>
            <ToggleGroupItem value="hour">Hour</ToggleGroupItem>
            <ToggleGroupItem value="day">Day</ToggleGroupItem>
            <ToggleGroupItem value="off">Off</ToggleGroupItem>
          </ToggleGroup>
        </Line>
        {draft.every === "off" ? null
          : <Line title="Keep"><Stepper name="Snapshots kept" count="keep" value={draft.keep} unit={keepSpanWords(draft)} onChange={(keep) => set({ keep })} /></Line>}
      </Section>
      <Section title="Space">
        <Line title="Up to"><Stepper name="Space limit" count="capGb" value={draft.capGb} unit="GB" onChange={(capGb) => set({ capGb })} /></Line>
      </Section>
      <p className="text-sm text-muted-foreground">
        Past the limit, the oldest automatic snapshots go first. Kept ones stay.{" "}
        {deletes > 0 ? <span className="text-destructive">Saving deletes {deletes} {deletes === 1 ? "snapshot" : "snapshots"} now.</span> : "Saving takes effect now."}
      </p>
    </div>
    <DialogFooter>
      <Button variant="outline" onClick={onCancel}>Cancel</Button>
      <Button variant={deletes > 0 ? "destructive" : "default"} disabled={pending}
        onClick={() => { void onSave(draft).then((saved) => { if (saved) onCancel(); }); }}>Save</Button>
    </DialogFooter>
  </>;
}

function Stepper({ name, count, value, unit, onChange }: {
  name: string;
  count: PolicyCount;
  value: number;
  unit: string;
  onChange: (value: number) => void;
}) {
  const [low, high] = POLICY_LIMITS[count];
  return <span className="flex items-center gap-2">
    <span className="flex items-center rounded-md border">
      <Button variant="ghost" size="icon-xs" aria-label={`Fewer ${name.toLowerCase()}`} disabled={value <= low} onClick={() => onChange(clampCount(count, value - 1))}><MinusIcon /></Button>
      <span aria-label={name} className="min-w-8 text-center text-sm tabular-nums">{value}</span>
      <Button variant="ghost" size="icon-xs" aria-label={`More ${name.toLowerCase()}`} disabled={value >= high} onClick={() => onChange(clampCount(count, value + 1))}><PlusIcon /></Button>
    </span>
    <span className="min-w-14 text-sm text-muted-foreground">{unit}</span>
  </span>;
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return <section className="flex flex-col gap-2">
    <h3 className="text-xs font-medium text-muted-foreground">{title}</h3>
    {children}
  </section>;
}

function Line({ title, children }: { title: string; children: ReactNode }) {
  return <Field orientation="horizontal" className="items-center justify-between">
    <FieldContent><FieldTitle>{title}</FieldTitle></FieldContent>
    {children}
  </Field>;
}
