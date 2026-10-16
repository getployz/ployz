import { useEffect, useRef, useState, type ReactNode } from "react";
import { Item, ItemActions } from "#/components/ui/item";
import { CARD_CHILD, SettingsAddButton, SettingsText } from "./settings-section";

const FIRST_CONTROL = "input, textarea, [data-slot=select-trigger], [data-slot=toggle-group-item], [data-slot=switch]";

/**
 * An optional setting most services never set. Closed it is one row with a `+ X`; open, it is the setting's real rows.
 * A configured one always renders open, and clearing it closes it again.
 */
export function SettingsDisclosure({ label, hint, addLabel, configured, children }: {
  label: ReactNode;
  hint?: ReactNode;
  addLabel: string;
  configured: boolean;
  /** The open rows, given `close` so a group's X can close it while nothing is set yet. */
  children: (close: () => void) => ReactNode;
}) {
  const [opened, setOpened] = useState(false);
  const [wasConfigured, setWasConfigured] = useState(configured);
  if (wasConfigured !== configured) {
    setWasConfigured(configured);
    setOpened(false);
  }
  const rows = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (opened) rows.current?.querySelector<HTMLElement>(FIRST_CONTROL)?.focus();
  }, [opened]);

  if (!configured && !opened) {
    return (
      <Item data-archetype="disclosure" className={CARD_CHILD}>
        <SettingsText label={label} hint={hint} />
        <ItemActions data-commit="none" className="justify-end">
          <SettingsAddButton onClick={() => setOpened(true)}>{addLabel}</SettingsAddButton>
        </ItemActions>
      </Item>
    );
  }
  const close = () => setOpened(false);
  return (
    <div
      ref={rows}
      data-archetype="disclosure"
      data-open=""
      className={`${CARD_CHILD} flex flex-col`}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !configured && !event.defaultPrevented) close();
      }}
    >
      {children(close)}
    </div>
  );
}
