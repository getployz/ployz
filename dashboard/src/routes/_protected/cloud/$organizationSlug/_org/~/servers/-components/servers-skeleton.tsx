import { Item, ItemActions, ItemContent, ItemGroup, ItemMedia } from "#/components/ui/item";
import { Skeleton } from "#/components/ui/skeleton";

export function ServersSkeleton() {
  return (
    <ItemGroup role="status" aria-label="Servers loading">
      {[0, 1, 2].map((row) => (
        <Item key={row} variant="outline" size="sm">
          <ItemMedia variant="icon"><Skeleton className="size-8" /></ItemMedia>
          <ItemContent>
            <Skeleton className="h-4 w-32" />
            <Skeleton className="h-3 w-64 max-w-full" />
          </ItemContent>
          <ItemActions><Skeleton className="h-4 w-16" /></ItemActions>
        </Item>
      ))}
    </ItemGroup>
  );
}
