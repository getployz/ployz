import { Link } from '@tanstack/react-router'

const footerSections = {
  Product: [
    { label: 'Features', to: '/features' },
    { label: 'Pricing', to: '/pricing' },
    { label: 'Canvas', to: '/features' },
    { label: 'PR Environments', to: '/features' },
  ],
  Developers: [
    { label: 'Documentation', to: '/docs' },
    { label: 'CLI Reference', to: '/docs' },
    { label: 'GitHub', href: 'https://github.com/getployz/ployz' },
    { label: 'Changelog', to: '/docs' },
  ],
  Company: [
    { label: 'About', to: '/about' },
    { label: 'Blog', to: '/blog' },
    { label: 'Contact', href: 'mailto:hello@ployz.dev' },
  ],
} as const

export default function Footer() {
  const year = new Date().getFullYear()

  return (
    <footer className="mt-32 border-t border-[var(--plz-border)] bg-[var(--plz-bg)]">
      <div className="page-wrap py-16">
        <div className="grid gap-12 sm:grid-cols-2 lg:grid-cols-4">
          {/* Brand */}
          <div>
            <div className="flex items-center gap-2.5">
              <div className="flex h-7 w-7 items-center justify-center rounded-md bg-gradient-to-br from-[var(--plz-accent)] to-[var(--color-plz-cyan)]">
                <span className="text-xs font-black text-white">P</span>
              </div>
              <span className="text-base font-bold text-[var(--plz-text)]">
                Ployz
              </span>
            </div>
            <p className="mt-4 max-w-xs text-sm leading-relaxed text-[var(--plz-text-muted)]">
              Deploy infrastructure that stays yours. Open source daemon,
              optional cloud dashboard. Eject anytime.
            </p>
          </div>

          {/* Link columns */}
          {Object.entries(footerSections).map(([title, links]) => (
            <div key={title}>
              <h4 className="mb-4 text-sm font-semibold text-[var(--plz-text)]">
                {title}
              </h4>
              <ul className="flex flex-col gap-2.5">
                {links.map((link) => (
                  <li key={link.label}>
                    {'href' in link ? (
                      <a
                        href={link.href}
                        target="_blank"
                        rel="noreferrer"
                        className="text-sm text-[var(--plz-text-muted)] transition hover:text-[var(--plz-text)]"
                      >
                        {link.label}
                      </a>
                    ) : (
                      <Link
                        to={link.to}
                        className="text-sm text-[var(--plz-text-muted)] transition hover:text-[var(--plz-text)]"
                      >
                        {link.label}
                      </Link>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>

        <div className="mt-12 flex flex-col items-center justify-between gap-4 border-t border-[var(--plz-border)] pt-8 sm:flex-row">
          <p className="text-xs text-[var(--plz-text-dim)]">
            &copy; {year} Ployz. All rights reserved.
          </p>
          <p className="text-xs text-[var(--plz-text-dim)]">
            Open source under MIT
          </p>
        </div>
      </div>
    </footer>
  )
}
