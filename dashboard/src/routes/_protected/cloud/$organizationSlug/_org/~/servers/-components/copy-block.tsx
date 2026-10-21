import { CopyButton } from "#/components/copy-button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { installLines, type InstallTarget } from "#/modules/machines/waiting/install-lines";

export function CopyBlock({ value }: { value: string }) {
  return (
    <div className="relative">
      <pre className="ph-no-capture max-h-56 overflow-auto rounded-md border border-border bg-muted p-3 pr-24 font-mono text-xs leading-relaxed text-foreground">
        <code className="whitespace-pre-wrap break-all">{value}</code>
      </pre>
      <CopyButton value={value} showLabel variant="outline" className="absolute top-2 right-2" />
    </div>
  );
}

export function InstallTabs({ target }: { target: InstallTarget }) {
  const lines = installLines(target);
  return (
    <Tabs defaultValue="shell">
      <TabsList>
        <TabsTrigger value="shell">Shell</TabsTrigger>
        <TabsTrigger value="cloud-init">cloud-init</TabsTrigger>
      </TabsList>
      <TabsContent value="shell" className="flex flex-col gap-2">
        <CopyBlock value={lines.shell} />
        <p className="text-sm text-muted-foreground">Paste this Fleet Token when it asks. It doesn't show what you paste.</p>
        <CopyBlock value={target.token} />
      </TabsContent>
      <TabsContent value="cloud-init"><CopyBlock value={lines.cloudInit} /></TabsContent>
    </Tabs>
  );
}
