import type { ReactNode } from "react";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemFooter, ItemHeader, ItemTitle } from "#/components/ui/item";

/**
 * How a row's control saves. `confirm` is a draft input that saves on its check or Enter, `change` saves the moment it
 * is picked or flipped, `form` saves its fields together at one Save, and `none` saves nothing: a value, a link or a
 * button that opens something else.
 */
export type SettingsCommit = "confirm" | "change" | "form" | "none";

/** A staged item: added by the next deploy (green), changed by it (pink), or removed by it (red, struck). */
export type SettingsTone = "new" | "changed" | "removed";

/**
 * Every card child draws its own top hairline. The card can't use divide-y: Item's own transparent border outranks
 * divide's zero-specificity rule, so the lines would vanish.
 */
export const CARD_CHILD = "rounded-none border-0 border-t border-border first:border-t-0";

const TONE = { new: "text-success", changed: "", removed: "text-destructive line-through" } satisfies Record<SettingsTone, string>;

/**
 * A changed row is pink edge to edge. It keeps the card's hairline and drops Item's ring, which would spill over its
 * neighbours; at the card's ends it follows the card's corners.
 */
const CHANGED_ROW = "data-[changed=true]:border-border data-[changed=true]:bg-changed-soft data-[changed=true]:ring-0 first:rounded-t-[calc(var(--radius-lg)-1px)] last:rounded-b-[calc(var(--radius-lg)-1px)]";

/** The attributes that paint a row by its tone: only a changed row paints the whole row, the rest tint its label. */
export function toneRow(tone: SettingsTone | undefined, className: string) {
  return tone === "changed" ? { "data-changed": true, className: `${className} ${CHANGED_ROW}` } : { className };
}

/** Inputs and selects fill the control column; switches and buttons sit at its right edge. */
const FILL = {
  confirm: "*:flex-1",
  change: "*:data-[slot=select-trigger]:flex-1",
  form: "*:flex-1",
  none: "",
} satisfies Record<SettingsCommit, string>;

/**
 * One section of settings, the same on every settings surface: a title, a line of description only where the title
 * isn't enough, and its rows in one bordered card that draws the hairlines between them. Below the card's 36rem the
 * control column wraps under its label.
 */
export function SettingsSection({ id, title, description, action, children }: {
  id: string;
  title: string;
  description?: ReactNode;
  /** A control beside the title that acts on the whole section, like New environment. */
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section data-sec={id} aria-labelledby={`settings-${id}`} className="flex flex-col gap-3">
      {/* The title never breaks: where the actions don't fit beside it, they wrap under it. */}
      <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-2">
        <div className="flex min-w-48 flex-1 flex-col gap-1">
          <h2 id={`settings-${id}`} className="text-base font-semibold">{title}</h2>
          {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
        </div>
        {action ? <div className="flex shrink-0 flex-wrap justify-end gap-2">{action}</div> : null}
      </div>
      <div data-slot="settings-card" className="@container/settings flex flex-col rounded-lg border bg-card">
        {children}
      </div>
    </section>
  );
}

/** A row's label and the hint under it, with anything that qualifies the row (a warning, an error) below. */
export function SettingsText({ label, hint, tone, children }: {
  label: ReactNode;
  hint?: ReactNode;
  tone?: SettingsTone;
  children?: ReactNode;
}) {
  return (
    <ItemContent className="min-w-48">
      <ItemTitle className={tone ? TONE[tone] : undefined}>{label}</ItemTitle>
      {hint ? <ItemDescription className="line-clamp-none">{hint}</ItemDescription> : null}
      {children}
    </ItemContent>
  );
}

/** The control column every row shares, so a screen keeps one control edge. Below 36rem it fills the row. */
export function SettingsControl({ commit, children }: { commit: SettingsCommit; children: ReactNode }) {
  return (
    <ItemActions data-commit={commit} className={`w-full justify-end @xl/settings:w-80 ${FILL[commit]}`}>
      {children}
    </ItemActions>
  );
}

/** A setting: label and hint left, its one control right. */
export function SettingsRow({ id, label, hint, tone, commit, control, children }: {
  /** A deep link target, like `#healthcheck`. */
  id?: string;
  label: ReactNode;
  hint?: ReactNode;
  tone?: SettingsTone;
  commit: SettingsCommit;
  control?: ReactNode;
  /** Under the hint: a warning or a note about this row. */
  children?: ReactNode;
}) {
  return (
    <Item id={id} data-archetype="row" {...toneRow(tone, id ? `${CARD_CHILD} scroll-mt-4` : CARD_CHILD)}>
      <SettingsText label={label} hint={hint} tone={tone}>{children}</SettingsText>
      {control ? <SettingsControl commit={commit}>{control}</SettingsControl> : null}
    </Item>
  );
}

/** A destructive action: what it does and its consequence, and the soft destructive button. Always the last section. */
export function SettingsDanger({ label, hint, action, children }: {
  label: ReactNode;
  hint: ReactNode;
  action: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Item data-archetype="danger" className={CARD_CHILD}>
      <SettingsText label={label} hint={hint}>{children}</SettingsText>
      <ItemActions data-commit="none" className="justify-end">{action}</ItemActions>
    </Item>
  );
}

/** A full-width row for an editor, a log or a file: its header line holds the tools, its body spans the card. */
export function SettingsWorkspace({ label, hint, tools, children }: {
  label: ReactNode;
  hint?: ReactNode;
  tools?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Item data-archetype="workspace" className={`${CARD_CHILD} flex-col items-stretch`}>
      <ItemHeader>
        <SettingsText label={label} hint={hint} />
        {tools ? <ItemActions>{tools}</ItemActions> : null}
      </ItemHeader>
      <ItemFooter className="*:min-w-0 *:flex-1">{children}</ItemFooter>
    </Item>
  );
}

/** The card's last row of small `+ X` buttons: a list's add, a source to pick, rare options to open. */
export function SettingsFoot({ children }: { children: ReactNode }) {
  return (
    <Item data-archetype="list-foot" className={CARD_CHILD}>
      <div className="flex flex-wrap gap-2">{children}</div>
    </Item>
  );
}

/** The `+ X` button of a foot or a closed disclosure. */
export function SettingsAddButton({ children, onClick, disabled, changed }: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  /** Pink: what it would add is staged to go. */
  changed?: boolean;
}) {
  return (
    <Button type="button" variant="outline" size="sm" disabled={disabled} data-changed={changed || undefined} onClick={onClick}>
      <PlusIcon data-icon="inline-start" />{children}
    </Button>
  );
}
