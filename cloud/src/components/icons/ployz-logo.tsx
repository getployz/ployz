import { cn } from '#/lib/utils'

const MARK_FILL = 'hsl(338 76% 56%)'

export function PloyzMark({
  className,
  decorative = false,
}: {
  className?: string;
  decorative?: boolean;
}) {
  return (
    <svg
      viewBox="0 0 64 64"
      xmlns="http://www.w3.org/2000/svg"
      role={decorative ? undefined : "img"}
      aria-label={decorative ? undefined : "Ployz"}
      aria-hidden={decorative || undefined}
      className={cn('size-8', className)}
    >
      <rect x="6" y="22" width="32" height="32" rx="8" fill={MARK_FILL} opacity="0.35" />
      <rect x="14" y="14" width="32" height="32" rx="8" fill={MARK_FILL} opacity="0.65" />
      <rect x="22" y="6" width="32" height="32" rx="8" fill={MARK_FILL} />
      <circle cx="48" cy="14" r="2.5" fill="#fff" />
    </svg>
  )
}

export function PloyzLogo({ className }: { className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-2', className)}>
      <PloyzMark className="size-7" />
      <span className="font-heading text-lg font-bold tracking-tight">
        ployz
      </span>
    </span>
  )
}
