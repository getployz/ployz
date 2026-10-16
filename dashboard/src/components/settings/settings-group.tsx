import type { ReactNode } from "react";
import { Item } from "#/components/ui/item";
import { CARD_CHILD, SettingsControl, SettingsText, toneRow, type SettingsCommit, type SettingsTone } from "./settings-section";

/**
 * One setting that owns several fields: a header like a row, with the group's off control (an X or a Switch) on the
 * right, and its fields on a left rule under it. The fields keep the rows' control column, so a screen keeps one edge.
 * Pass no children to hide the fields, as a Switch group does while off.
 */
export function SettingsGroup({ id, label, hint, tone, commit, control, children }: {
  id?: string;
  label: ReactNode;
  hint?: ReactNode;
  tone?: SettingsTone;
  /** How the header control saves. An X that turns the setting off, or a Switch, is `change`. */
  commit: SettingsCommit;
  control?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div id={id} data-archetype="group" {...toneRow(tone, `${CARD_CHILD} flex flex-col${id ? " scroll-mt-4" : ""}`)}>
      <Item className="rounded-none border-0">
        <SettingsText label={label} hint={hint} tone={tone} />
        {control ? <SettingsControl commit={commit}>{control}</SettingsControl> : null}
      </Item>
      {children ? <div className="mb-2.5 ml-3 flex flex-col border-l-2">{children}</div> : null}
    </div>
  );
}

/** One field of a group: a label and its control in the rows' column. The group's hint covers it. */
export function SettingsField({ label, htmlFor, commit, control, children }: {
  label: ReactNode;
  htmlFor?: string;
  commit: SettingsCommit;
  control: ReactNode;
  /** Under the field: what went wrong with it. */
  children?: ReactNode;
}) {
  return (
    <div data-slot="settings-field" className="flex flex-wrap items-center gap-2.5 px-3 py-1">
      <label htmlFor={htmlFor} className="min-w-32 flex-1 text-sm">{label}</label>
      <SettingsControl commit={commit}>{control}</SettingsControl>
      {children ? <div className="basis-full">{children}</div> : null}
    </div>
  );
}
