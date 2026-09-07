import { Link } from '@tanstack/react-router'
import { PloyzLogo } from '#/components/icons/ployz-logo'
import { MarketingPrimaryAction } from '#/routes/_public/-components/MarketingPrimaryAction'

export default function Header() {
  return (
    <header className="marketing-header">
      <nav className="marketing-frame marketing-nav" aria-label="Primary">
        <Link to="/home" aria-label="Ployz home">
          <PloyzLogo />
        </Link>
        <MarketingPrimaryAction />
      </nav>
    </header>
  )
}
