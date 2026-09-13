import { useId } from "react";

// ponytail: hand-drawn 32x32 mark, no logo library. Monochrome via currentColor;
// the skull is masked out of the flag so it works on any background.
export function PirateFlag({ className }: { className?: string }) {
  const mask = useId();
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
      <defs>
        <mask id={mask}>
          <rect width="32" height="32" fill="white" />
          <g fill="black">
            <circle cx="16.6" cy="11" r="3.9" />
            <rect x="14.4" y="13.6" width="4.4" height="2.4" rx="0.8" />
            <path
              d="M11 20.6l11-6.4M11 14.2l11 6.4"
              stroke="black"
              strokeWidth="1.7"
              strokeLinecap="round"
            />
            <circle cx="11" cy="20.6" r="1.1" />
            <circle cx="22" cy="14.2" r="1.1" />
            <circle cx="11" cy="14.2" r="1.1" />
            <circle cx="22" cy="20.6" r="1.1" />
          </g>
          <g fill="white">
            <circle cx="15.1" cy="10.6" r="1.05" />
            <circle cx="18.1" cy="10.6" r="1.05" />
            <path d="M16.6 12.1l-.7 1.2h1.4Z" />
            <rect x="15.6" y="14.1" width="0.7" height="1.6" />
            <rect x="16.9" y="14.1" width="0.7" height="1.6" />
          </g>
        </mask>
      </defs>
      <rect x="2" y="3" width="2.6" height="27" rx="1" fill="currentColor" />
      <circle cx="3.3" cy="2.6" r="2.4" fill="var(--pirate-coral, #f37756)" />
      <path
        d="M4.6 5H29.2c-2.2 2.9 1.6 5.6-.4 8.5s1.6 5.6-.4 8.5H4.6Z"
        fill="currentColor"
        mask={`url(#${mask})`}
      />
    </svg>
  );
}
