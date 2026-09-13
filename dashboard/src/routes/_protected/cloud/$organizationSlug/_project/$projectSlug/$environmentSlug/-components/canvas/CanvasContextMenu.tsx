import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuGroup,
  ContextMenuItem,
  ContextMenuTrigger,
} from "#/components/ui/context-menu";
import { SERVICE_CREATE_MENU_ITEMS } from "#/components/create-menu-items";
import type { CreateMenuItemId } from "#/components/create-menu-items";
import type { CreatorPanel } from "./types";

function getActionForItem(
  itemId: CreateMenuItemId,
  actions: {
    onCreateFromPanel: (panel: CreatorPanel) => void;
    onCreateBlank: () => void;
    onCreateVariableGroup: () => void;
    onCreateVolume: () => void;
  },
) {
  if (itemId === "git-repository") {
    return () => actions.onCreateFromPanel("git");
  }
  if (itemId === "container-image") {
    return () => actions.onCreateFromPanel("image");
  }
  if (itemId === "empty-service") {
    return actions.onCreateBlank;
  }
  if (itemId === "variable-group") {
    return actions.onCreateVariableGroup;
  }
  if (itemId === "volume") {
    return actions.onCreateVolume;
  }

  return undefined;
}

export function CanvasContextMenu({
  children,
  onCreateFromPanel,
  onCreateBlank,
  onCreateVariableGroup,
  onCreateVolume,
}: {
  children: React.ReactNode;
  onCreateFromPanel: (panel: CreatorPanel) => void;
  onCreateBlank: () => void;
  onCreateVariableGroup: () => void;
  onCreateVolume: () => void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger className="h-full w-full">
        {children}
      </ContextMenuTrigger>
      <ContextMenuContent style={{ width: 220 }}>
        <ContextMenuGroup>
          {SERVICE_CREATE_MENU_ITEMS.map(({ id, icon: Icon, label }) => (
            <ContextMenuItem
              key={id}
              onClick={getActionForItem(id, {
                onCreateFromPanel,
                onCreateBlank,
                onCreateVariableGroup,
                onCreateVolume,
              })}
            >
              <Icon />
              {label}
            </ContextMenuItem>
          ))}
        </ContextMenuGroup>
      </ContextMenuContent>
    </ContextMenu>
  );
}
