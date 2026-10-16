import { useState, type ReactNode } from "react";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Item, ItemActions, ItemMedia } from "#/components/ui/item";
import { Label } from "#/components/ui/label";
import { Spinner } from "#/components/ui/spinner";
import { CARD_CHILD, SettingsAddButton, SettingsFoot, SettingsText, type SettingsCommit, type SettingsTone } from "./settings-section";

/** Several things of one kind, each a row with its media, so every label in the list starts at the same edge. */
export function SettingsList({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div role="list" aria-label={label} data-archetype="list" className={`${CARD_CHILD} flex flex-col`}>
      {children}
    </div>
  );
}

/** One thing in a list: its icon or avatar, its name and a line about it, and its actions at the right edge. */
export function SettingsListItem({ media, label, hint, tone, commit, actions, children }: {
  media: ReactNode;
  label: ReactNode;
  hint?: ReactNode;
  tone?: SettingsTone;
  /** How the actions save: a remove that stages at once is `change`; a menu that only navigates is `none`. */
  commit: SettingsCommit;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Item role="listitem" data-archetype="list-item" className={CARD_CHILD}>
      <ItemMedia variant="icon">{media}</ItemMedia>
      <SettingsText label={label} hint={hint} tone={tone}>{children}</SettingsText>
      {actions ? <ItemActions data-commit={commit} className="justify-end">{actions}</ItemActions> : null}
    </Item>
  );
}

/** A list with nothing in it yet: what would be here and why, left aligned. Its add stays in the foot below. */
export function SettingsEmpty({ title, children }: { title: ReactNode; children?: ReactNode }) {
  return (
    <Item data-archetype="empty" className={CARD_CHILD}>
      <Empty variant="placeholder">
        <EmptyHeader>
          <EmptyTitle>{title}</EmptyTitle>
          {children ? <EmptyDescription>{children}</EmptyDescription> : null}
        </EmptyHeader>
      </Empty>
    </Item>
  );
}

/**
 * Fields that only mean something together, saved by one Save in the card's foot row. Escape or Cancel throws the
 * entries away; a failed save keeps them.
 */
export function SettingsForm({ submitLabel, onSubmit, onCancel, pending, disabled, error, children }: {
  submitLabel: string;
  onSubmit: () => void;
  onCancel: () => void;
  pending?: boolean;
  disabled?: boolean;
  error?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Item
      data-archetype="list-foot"
      data-commit="form"
      className={`${CARD_CHILD} items-end`}
      render={(
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (!disabled && !pending) onSubmit();
          }}
          onKeyDown={(event) => {
            if (event.key !== "Escape" || event.defaultPrevented) return;
            event.preventDefault();
            onCancel();
          }}
        />
      )}
    >
      <div className="flex min-w-0 flex-1 flex-wrap items-end gap-2.5 *:min-w-48 *:flex-1">{children}</div>
      <div className="flex gap-2">
        <Button type="button" variant="ghost" onClick={onCancel}>Cancel</Button>
        <Button type="submit" disabled={disabled || pending}>
          {pending ? <Spinner data-icon="inline-start" /> : null}{submitLabel}
        </Button>
      </div>
      {error ? <div role="alert" className="basis-full text-sm text-destructive">{error}</div> : null}
    </Item>
  );
}

/**
 * One entry of a SettingsForm: its label over its control, or after it when `inline` (a checkbox reads as one line).
 */
export function SettingsFormField({ label, htmlFor, inline, children }: {
  label: ReactNode;
  htmlFor: string;
  inline?: boolean;
  children: ReactNode;
}) {
  if (inline) {
    return (
      <div data-slot="settings-form-field" className="flex items-center gap-2">
        {children}
        <Label htmlFor={htmlFor} className="font-normal">{label}</Label>
      </div>
    );
  }
  return (
    <div data-slot="settings-form-field" className="flex flex-col gap-1.5">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
    </div>
  );
}

/**
 * A list's add, in its foot. Closed it is one `+ X`; open, the foot itself becomes the form, which `children` renders
 * as a SettingsForm and closes when it is done.
 */
export function SettingsListAdd({ label, disabled, changed, children }: {
  label: string;
  disabled?: boolean;
  changed?: boolean;
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  if (open) return children(() => setOpen(false));
  return (
    <SettingsFoot>
      <SettingsAddButton disabled={disabled} changed={changed} onClick={() => setOpen(true)}>{label}</SettingsAddButton>
    </SettingsFoot>
  );
}
