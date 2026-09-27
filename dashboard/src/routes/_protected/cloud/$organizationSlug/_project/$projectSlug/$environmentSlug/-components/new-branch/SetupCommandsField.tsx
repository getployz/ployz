import { PlusIcon, XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { SetupCommand } from "#/modules/project/tables";

/** Commands and the service each runs in. `typed` marks a keystroke, so a saved-at-once owner can wait for blur. */
export function SetupCommandsField({ id, commands, services, onChange, onBlur }: {
  id: string;
  commands: SetupCommand[];
  services: Array<{ lineageId: string; name: string }>;
  onChange: (next: SetupCommand[], typed?: boolean) => void;
  onBlur?: () => void;
}) {
  const nameOf = (lineage: string) => services.find((service) => service.lineageId === lineage)?.name ?? "a service";
  const at = (index: number, change: Partial<SetupCommand>, typed?: boolean) =>
    onChange(commands.map((setup, i) => i === index ? { ...setup, ...change } : setup), typed);
  const first = services[0];
  return (
    <div className="flex flex-col gap-2">
      {commands.map((setup, index) => (
        <div key={index} className="flex items-center gap-2">
          <Input id={index === 0 ? id : undefined} aria-label="Command" className="min-w-0 flex-1 font-mono"
            placeholder="pnpm db:seed" spellCheck={false} autoComplete="off" value={setup.command}
            onChange={(event) => at(index, { command: event.target.value }, true)} onBlur={onBlur} />
          <Select value={setup.lineageId} onValueChange={(lineageId) => { if (lineageId) at(index, { lineageId }); }}>
            <SelectTrigger aria-label="Runs in" className="shrink-0">
              <SelectValue><span className="text-muted-foreground">in</span> {nameOf(setup.lineageId)}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {services.map((service) => (
                  <SelectItem key={service.lineageId} value={service.lineageId} label={service.name}>{service.name}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <Tooltip>
            <TooltipTrigger render={
              <Button type="button" variant="ghost" size="icon" aria-label="Remove this command"
                onClick={() => onChange(commands.filter((_, i) => i !== index))} />
            }><XIcon /></TooltipTrigger>
            <TooltipContent>Remove this command</TooltipContent>
          </Tooltip>
        </div>
      ))}
      {first && (
        <Button type="button" variant="outline" className="self-start"
          onClick={() => onChange([...commands, { lineageId: first.lineageId, command: "" }], true)}>
          <PlusIcon data-icon="inline-start" />Add a command
        </Button>
      )}
    </div>
  );
}
