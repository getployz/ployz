import { CopyButton } from "#/components/copy-button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import type { installLines } from "#/modules/machines/waiting/install-lines";

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

export function InstallTabs({ lines }: { lines: ReturnType<typeof installLines> }) {
  return (
    <Tabs defaultValue="shell">
      <TabsList>
        <TabsTrigger value="shell">Shell</TabsTrigger>
        <TabsTrigger value="cloud-init">cloud-init</TabsTrigger>
        <TabsTrigger value="terraform">Terraform</TabsTrigger>
      </TabsList>
      <TabsContent value="shell"><CopyBlock value={lines.shell} /></TabsContent>
      <TabsContent value="cloud-init"><CopyBlock value={lines.cloudInit} /></TabsContent>
      <TabsContent value="terraform"><CopyBlock value={lines.terraform} /></TabsContent>
    </Tabs>
  );
}
