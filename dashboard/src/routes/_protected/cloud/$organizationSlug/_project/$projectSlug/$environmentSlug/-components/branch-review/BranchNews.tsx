import { useState, type ReactNode } from "react";
import { ChevronDownIcon } from "lucide-react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";

/** One line of news: what, in a few words, its one action, and its changes to open. The lead is a card. */
export function NewsRow({ lead = false, icon, title, detail, action, changes }: {
  lead?: boolean;
  icon: ReactNode;
  title: string;
  detail?: ReactNode;
  action?: ReactNode;
  changes?: ReactNode[];
}) {
  const [open, setOpen] = useState(false);
  const text = (
    <>
      <ItemMedia variant="icon">{icon}</ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>
          {title}
          {changes?.length ? <ChevronDownIcon aria-hidden="true" className={cn("size-4 text-muted-foreground transition-transform", !open && "-rotate-90")} /> : null}
        </ItemTitle>
        {detail ? <ItemDescription className="truncate">{detail}</ItemDescription> : null}
      </ItemContent>
    </>
  );
  const row = (
    <Item variant={lead ? "outline" : "default"} size="sm">
      {changes?.length ? (
        <CollapsibleTrigger render={<button type="button" className="flex min-w-0 flex-1 items-center gap-2.5 text-left" />}>{text}</CollapsibleTrigger>
      ) : text}
      {action ? <ItemActions>{action}</ItemActions> : null}
      {changes?.length ? <CollapsibleContent className="basis-full"><ItemGroup className="gap-1">{changes}</ItemGroup></CollapsibleContent> : null}
    </Item>
  );
  return changes?.length ? <Collapsible open={open} onOpenChange={setOpen}>{row}</Collapsible> : row;
}

/** The lead's action is the panel's one solid button. */
export const actionVariant = (lead: boolean) => lead ? "default" : "outline";
