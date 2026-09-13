interface CodeBlockProps {
  code: string
  label?: string
}

export function CodeBlock({ code, label }: CodeBlockProps) {
  return (
    <div className="overflow-hidden rounded-[1.5rem] border bg-card shadow-sm">
      <div className="flex items-center gap-2 border-b bg-muted/60 px-4 py-3">
        <span className="size-2 rounded-full bg-foreground/20" />
        <span className="size-2 rounded-full bg-foreground/12" />
        <span className="size-2 rounded-full bg-foreground/8" />
        <span className="ml-2 text-xs font-medium text-muted-foreground">
          {label ?? 'Terminal'}
        </span>
      </div>
      <pre className="overflow-x-auto bg-background px-4 py-5 text-sm font-mono leading-relaxed">
        <code>{code}</code>
      </pre>
    </div>
  )
}
