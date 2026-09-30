import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuGroup,
  ContextMenuItem,
  ContextMenuTrigger,
} from "#/components/ui/context-menu";
import { SERVICE_CREATE_MENU_ITEMS } from "#/components/create-menu-items";
import type { CreateMenuItem } from "#/components/create-menu-items";
import type { CreatorPanel } from "./types";

function getActionForItem(
  { id, panel }: CreateMenuItem,
  actions: {
    onCreateFromPanel: (panel: CreatorPanel) => void;
    onCreateBlank: () => void;
    onCreateVolume: () => void;
  },
) {
  if (panel) {
    return () => actions.onCreateFromPanel(panel);
  }
  if (id === "empty-service") {
    return actions.onCreateBlank;
  }
  if (id === "volume") {
    return actions.onCreateVolume;
  }

  return undefined;
}

export function CanvasContextMenu({
  children,
  onCreateFromPanel,
  onCreateBlank,
  onCreateVolume,
}: {
  children: React.ReactNode;
  onCreateFromPanel: (panel: CreatorPanel) => void;
  onCreateBlank: () => void;
  onCreateVolume: () => void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger className="h-full w-full">
        {children}
      </ContextMenuTrigger>
      <ContextMenuContent style={{ width: 220 }}>
        <ContextMenuGroup>
          {SERVICE_CREATE_MENU_ITEMS.map((item) => (
            <ContextMenuItem
              key={item.id}
              onClick={getActionForItem(item, {
                onCreateFromPanel,
                onCreateBlank,
                onCreateVolume,
              })}
            >
              <item.icon />
              {item.label}
            </ContextMenuItem>
          ))}
        </ContextMenuGroup>
      </ContextMenuContent>
    </ContextMenu>
  );
}
