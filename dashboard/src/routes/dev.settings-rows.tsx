import { useId, useState } from "react";
import { createFileRoute, notFound } from "@tanstack/react-router";
import { GlobeIcon, LockIcon, XIcon } from "lucide-react";
import { RowWarning } from "#/components/settings/row-warning";
import { SettingsDisclosure } from "#/components/settings/settings-disclosure";
import { SettingsField, SettingsGroup } from "#/components/settings/settings-group";
import { SettingsEmpty, SettingsForm, SettingsFormField, SettingsList, SettingsListAdd, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsAddButton, SettingsDanger, SettingsFoot, SettingsRow, SettingsSection, SettingsWorkspace } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";

// Every settings archetype in every state, light beside dark, for eyeballing and for the measured verify loop.
export const Route = createFileRoute("/dev/settings-rows")({
  beforeLoad: () => {
    if (!import.meta.env.DEV) throw notFound();
  },
  head: () => ({ meta: [{ title: "Settings rows" }] }),
  component: SettingsRowsGallery,
});

function SettingsRowsGallery() {
  return (
    <main className="grid min-h-screen grid-cols-1 xl:grid-cols-2">
      <div className="light bg-background p-8 text-foreground" data-theme="light"><Gallery /></div>
      <div className="dark bg-background p-8 text-foreground" data-theme="dark"><Gallery /></div>
    </main>
  );
}

const REGIONS = ["Falkenstein", "Helsinki", "Ashburn"];

function Gallery() {
  const [name, setName] = useState("api");
  const [draft, setDraft] = useState(name);
  const id = useId();
  const [region, setRegion] = useState("Falkenstein");
  const [sleep, setSleep] = useState(false);
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-10">
      <SettingsSection id="row" title="Rows" description="One setting each. The control column keeps one edge.">
        <SettingsRow
          label="Service name"
          hint="Shown on the canvas and in logs."
          commit="confirm"
          control={(
            <ConfirmableInput aria-label="Service name" value={draft} isDirty={draft !== name} onValueChange={setDraft}
              onConfirm={() => setName(draft)} onCancel={() => setDraft(name)} />
          )}
        />
        <SettingsRow
          label="Start command"
          hint="Staged for the next deploy."
          commit="confirm"
          control={<ConfirmableInput aria-label="Start command" isChanged value="node server.js" onValueChange={() => {}} />}
        />
        <SettingsRow
          label="Region"
          hint="Where new replicas start."
          commit="change"
          control={(
            <Select value={region} onValueChange={(next) => { if (next) setRegion(next); }}>
              <SelectTrigger aria-label="Region"><SelectValue>{region}</SelectValue></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {REGIONS.map((item) => <SelectItem key={item} value={item} label={item}>{item}</SelectItem>)}
                </SelectGroup>
              </SelectContent>
            </Select>
          )}
        />
        <SettingsRow
          label="Sleep when idle"
          hint="Stops the service after ten minutes without requests."
          commit="change"
          control={<Switch aria-label="Sleep when idle" checked={sleep} onCheckedChange={setSleep} />}
        />
        <SettingsRow label="Replicas" hint="Runs 3 on this volume." commit="confirm" control={<Input aria-label="Replicas" defaultValue="3" />}>
          <RowWarning why="Several writers on one volume can corrupt a database.">Shares one volume</RowWarning>
        </SettingsRow>
        <SettingsRow label="Old port" hint="Removed on your next deploy." tone="removed" commit="none" />
        <SettingsRow label="New port" hint="Added on your next deploy." tone="new" commit="none" />
        <SettingsRow label="Private address" hint="api.internal" commit="none" control={<Button variant="outline" size="sm">Copy</Button>} />
      </SettingsSection>

      <SettingsSection id="confirm" title="Confirm states" description="One confirm row through its write: idle, edited, saving, staged, refused.">
        <SettingsRow label="Idle" hint="Stored value, nothing typed." commit="confirm"
          control={<ConfirmableInput aria-label="Idle" value="3" onValueChange={() => {}} />} />
        <SettingsRow label="Editing" hint="Enter or ✓ writes, Escape or ✕ reverts." commit="confirm"
          control={<ConfirmableInput aria-label="Editing" value="4" isDirty onValueChange={() => {}} />} />
        <SettingsRow label="Saving" hint="The write is on its way." commit="confirm"
          control={<ConfirmableInput aria-label="Saving" value="4" status={{ phase: "saving" }} onValueChange={() => {}} />} />
        <SettingsRow label="Staged" hint="Saved, waits for the next deploy." commit="confirm"
          control={<ConfirmableInput aria-label="Staged" value="4" isChanged onValueChange={() => {}} />} />
        <SettingsRow label="Failed" hint="Refused: the stored value is back." commit="confirm"
          control={(
            <ConfirmableInput aria-label="Failed" value="3" onValueChange={() => {}}
              status={{ phase: "failed", reason: "Replicas must be at most 50.", onRetry: () => {} }} />
          )} />
      </SettingsSection>

      <SettingsSection id="list" title="Lists" action={<Button variant="outline" size="sm">Section action</Button>}>
        <SettingsList label="Domains">
          <SettingsListItem media={<GlobeIcon />} label="api.example.com" hint="→ 8080 · Live" commit="change" actions={<Button variant="ghost" size="sm">Remove</Button>} />
          <SettingsListItem media={<GlobeIcon />} label="old.example.com" hint="Removed on your next deploy." tone="removed" commit="none" />
          <SettingsListItem media={<LockIcon />} label="new.example.com" hint="Added on your next deploy." tone="new" commit="none" />
        </SettingsList>
        <SettingsFoot>
          <SettingsAddButton onClick={() => {}}>Generate domain</SettingsAddButton>
          <SettingsAddButton changed onClick={() => {}}>Custom domain</SettingsAddButton>
        </SettingsFoot>
      </SettingsSection>

      <SettingsSection id="empty" title="Empty list">
        <SettingsEmpty title="No public domains">Generate one under the cluster domain, or add your own.</SettingsEmpty>
        <SettingsListAdd label="Add member">
          {(close) => (
            <SettingsForm submitLabel="Invite" onSubmit={close} onCancel={close}>
              <SettingsFormField label="Email" htmlFor={`${id}-email`}>
                <Input id={`${id}-email`} placeholder="name@example.com" />
              </SettingsFormField>
            </SettingsForm>
          )}
        </SettingsListAdd>
      </SettingsSection>

      <SettingsSection id="form" title="Form">
        <SettingsForm submitLabel="Save" onSubmit={() => {}} onCancel={() => {}} error="That name is taken.">
          <SettingsFormField label="Key" htmlFor={`${id}-key`}><Input id={`${id}-key`} placeholder="KEY" /></SettingsFormField>
          <SettingsFormField label="Value" htmlFor={`${id}-value`}><Input id={`${id}-value`} placeholder="value" /></SettingsFormField>
          <SettingsFormField label="Secret" htmlFor={`${id}-secret`} inline><Checkbox id={`${id}-secret`} /></SettingsFormField>
        </SettingsForm>
      </SettingsSection>

      <SettingsSection id="group" title="Groups and disclosures">
        <SettingsGroup
          label="Healthcheck"
          hint="Traffic waits until this path answers."
          commit="change"
          control={<Button variant="ghost" size="icon-sm" aria-label="Remove healthcheck"><XIcon /></Button>}
        >
          <SettingsField label="Path" commit="confirm" control={<Input aria-label="Path" defaultValue="/health" />} />
          <SettingsField label="Timeout" commit="confirm" control={<Input aria-label="Timeout" data-changed defaultValue="30s" />} />
        </SettingsGroup>
        <SettingsDisclosure label="Cron schedule" hint="Run on a timer instead of always." addLabel="Cron schedule" configured={false}>
          {() => <SettingsRow label="Cron schedule" commit="confirm" control={<Input aria-label="Cron schedule" placeholder="*/5 * * * *" />} />}
        </SettingsDisclosure>
        <SettingsWorkspace label="Config file" hint="Mounted at /etc/app.toml." tools={<Button variant="outline" size="sm">Upload</Button>}>
          <pre className="w-full rounded-md bg-muted p-3 font-mono text-xs">port = 8080{"\n"}workers = 4</pre>
        </SettingsWorkspace>
      </SettingsSection>

      <SettingsSection id="danger" title="Danger">
        <SettingsDanger
          label="Delete service"
          hint="Removes it from this environment on your next deploy."
          action={<Button variant="destructive" size="sm">Delete</Button>}
        />
      </SettingsSection>
    </div>
  );
}
