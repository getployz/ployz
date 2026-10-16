import { useState, type ReactNode } from "react";
import { PlusIcon, XIcon } from "lucide-react";
import type { JsonValue, ServiceSettingChange } from "@ployz/sdk";
import type { Persistable } from "#/collections/query-collection";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { InputGroupButton } from "#/components/ui/input-group";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { SettingsDisclosure } from "#/components/settings/settings-disclosure";
import { SettingsField, SettingsGroup } from "#/components/settings/settings-group";
import { serviceSetting, settingError } from "#/modules/config-store/catalog";
import { healthcheckOf, healthcheckValue, type Healthcheck, type HealthcheckKind } from "#/modules/config-store/healthcheck";
import { shownValue } from "#/modules/config-store/store-deployments";
import { changedProps } from "#/modules/config-store/store-services";
import { ServiceSettingInput } from "./ServiceSettingInput";

const { path, command, timeoutSeconds } = serviceSetting("healthcheck").properties;

const KINDS = {
  path: {
    add: "HTTP path", label: "Healthcheck path", field: "Path",
    chip: <>HTTP <span className="text-muted-foreground">GET</span></>,
    validate: (raw) => raw.startsWith("/") ? settingError({ ...path, title: "path", description: "", type: "string" }, raw) : "Start the path with /.",
  },
  command: {
    add: "Command", label: "Healthcheck command", field: "Command", chip: "CMD",
    validate: (raw) => settingError({ ...command, title: "command", description: "", type: "string" }, raw),
  },
} satisfies Record<HealthcheckKind, { add: string; label: string; field: string; chip: ReactNode; validate: (raw: string) => string | null }>;

const shown = (value: JsonValue) => shownValue(value) || "off";

type Draft = { kind: HealthcheckKind; text: string; error: string | null };

export type HealthcheckSetting = {
  value: JsonValue | undefined;
  change: ServiceSettingChange | undefined;
  /** Sets the Setting, or unsets it (null). */
  set: (value: JsonValue | null) => Persistable;
};

type Props = HealthcheckSetting & {
  label?: string;
  description?: ReactNode;
  /** Shows the timeout row; Monitoring leaves it out. */
  timeout?: boolean;
};

/**
 * A Service's healthcheck: off, two buttons; on, its path or command after a chip saying which, with an × that turns
 * it off, and its timeout below. Commits whole values, `{path|command, timeoutSeconds}`, the shape the Store shows.
 */
export function HealthcheckField(props: Props) {
  const check = healthcheckOf(props.value);
  return <HealthcheckFields key={check ? `${check.kind}:${check.text}` : ""} check={check} {...props} />;
}

function HealthcheckFields({ check, change, set, label = "Healthcheck", description = "A new replica must pass it before it takes traffic.",
  timeout = true }: Props & { check: Healthcheck | null }) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const kind = draft?.kind ?? check?.kind ?? null;
  const text = draft?.text ?? check?.text ?? "";
  const isDirty = draft !== null && (check === null || draft.kind !== check.kind || draft.text !== check.text);

  function confirm(kind: HealthcheckKind) {
    const next = text.trim();
    if (next === "") {
      if (check) set(null);
      return setDraft(null);
    }
    const error = KINDS[kind].validate(next);
    if (error) return setDraft({ kind, text, error });
    set(healthcheckValue({ kind, text: next, timeoutSeconds: check?.timeoutSeconds ?? timeoutSeconds.default }));
    setDraft({ kind, text: next, error: null });
  }

  return (
    <>
      <Field id="healthcheck" className="scroll-mt-4">
        <FieldLabel>{label}</FieldLabel>
        <FieldDescription>{description}</FieldDescription>
        {kind === null ? (
          <div className="grid grid-cols-2 gap-2">
            {(["path", "command"] as const).map((option) => (
              <Button key={option} type="button" variant="outline" data-changed={change ? true : undefined}
                onClick={() => setDraft({ kind: option, text: "", error: null })}>
                <PlusIcon data-icon="inline-start" />{KINDS[option].add}
              </Button>
            ))}
          </div>
        ) : (
          <ConfirmableInput aria-label={KINDS[kind].label} aria-invalid={draft?.error ? true : undefined} error={draft?.error}
            isChanged={change !== undefined} isDirty={isDirty} autoFocus={check === null}
            title={change ? `Deployed: ${shown(change.before)}` : undefined}
            startAddon={<Badge variant="secondary">{KINDS[kind].chip}</Badge>}
            endAddon={check && !isDirty ? (
              <InputGroupButton size="icon-xs" variant="ghost" aria-label="Remove healthcheck" onClick={() => set(null)}>
                <XIcon />
              </InputGroupButton>
            ) : null}
            value={text} onValueChange={(next) => setDraft({ kind, text: next, error: null })}
            onCancel={() => setDraft(null)} onConfirm={() => confirm(kind)} />
        )}
      </Field>
      {check && timeout ? (
        <Field orientation="responsive">
          <FieldContent><FieldLabel>Healthcheck timeout</FieldLabel></FieldContent>
          <div className="@md/field-group:basis-56 @md/field-group:shrink-0">
            <ServiceSettingInput ariaLabel="Healthcheck timeout" inputMode="numeric" suffix="seconds" placeholder={String(timeoutSeconds.default)}
              value={String(check.timeoutSeconds)} {...changedProps(change, shown)}
              validate={(raw) => settingError({ ...timeoutSeconds, title: "timeout", description: "", type: "integer" }, raw)}
              onCommit={(raw) => set(healthcheckValue({ ...check, timeoutSeconds: raw === "" ? timeoutSeconds.default : Number(raw) }))} />
          </div>
        </Field>
      ) : null}
    </>
  );
}

const HINT = "A new replica must pass it before it takes traffic.";

/**
 * The Settings tab's healthcheck: a disclosure most Services leave closed, open a group whose X turns it off. Its check
 * kind, its path or command, and its timeout are the group's fields. Monitoring keeps the inline field above.
 */
export function HealthcheckSettings(props: HealthcheckSetting) {
  const check = healthcheckOf(props.value);
  return (
    <SettingsDisclosure label="Healthcheck" hint={HINT} addLabel="Healthcheck" configured={check !== null}>
      {(close) => <HealthcheckGroup key={check ? `${check.kind}:${check.text}` : ""} check={check} close={close} {...props} />}
    </SettingsDisclosure>
  );
}

function HealthcheckGroup({ check, change, set, close }: HealthcheckSetting & { check: Healthcheck | null; close: () => void }) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const kind = draft?.kind ?? check?.kind ?? "path";
  const text = draft?.text ?? check?.text ?? "";
  const isDirty = draft !== null && (check === null || draft.kind !== check.kind || draft.text !== check.text);

  function confirm() {
    const next = text.trim();
    if (next === "") {
      if (check) set(null);
      return setDraft(null);
    }
    const error = KINDS[kind].validate(next);
    if (error) return setDraft({ kind, text, error });
    set(healthcheckValue({ kind, text: next, timeoutSeconds: check?.timeoutSeconds ?? timeoutSeconds.default }));
    setDraft({ kind, text: next, error: null });
  }

  return (
    <SettingsGroup id="healthcheck" label="Healthcheck" hint={HINT} commit="change" control={
      <Button type="button" variant="ghost" size="icon-sm" aria-label="Remove healthcheck" onClick={() => (check ? set(null) : close())}>
        <XIcon />
      </Button>
    }>
      <SettingsField label="Check" commit="change" control={
        <ToggleGroup variant="outline" size="sm" spacing={0} value={[kind]} aria-label="Check"
          onValueChange={([next]) => {
            if ((next === "path" || next === "command") && next !== kind) setDraft({ kind: next, text: "", error: null });
          }}>
          {(["path", "command"] as const).map((option) => <ToggleGroupItem key={option} value={option}>{KINDS[option].add}</ToggleGroupItem>)}
        </ToggleGroup>
      } />
      <SettingsField label={KINDS[kind].field} commit="confirm" control={
        <ConfirmableInput aria-label={KINDS[kind].label} aria-invalid={draft?.error ? true : undefined} error={draft?.error}
          isChanged={change !== undefined} isDirty={isDirty} title={change ? `Deployed: ${shown(change.before)}` : undefined}
          placeholder={kind === "path" ? "/health" : "pg_isready"}
          value={text} onValueChange={(next) => setDraft({ kind, text: next, error: null })}
          onCancel={() => setDraft(null)} onConfirm={confirm} />
      } />
      {check ? (
        <SettingsField label="Timeout" commit="confirm" control={
          <ServiceSettingInput ariaLabel="Healthcheck timeout" inputMode="numeric" suffix="seconds" placeholder={String(timeoutSeconds.default)}
            value={String(check.timeoutSeconds)} {...changedProps(change, shown)}
            validate={(raw) => settingError({ ...timeoutSeconds, title: "timeout", description: "", type: "integer" }, raw)}
            onCommit={(raw) => set(healthcheckValue({ ...check, timeoutSeconds: raw === "" ? timeoutSeconds.default : Number(raw) }))} />
        } />
      ) : null}
    </SettingsGroup>
  );
}
