import { Link } from '@tanstack/react-router'
import { PloyzLogo } from '#/components/icons/ployz-logo'

export default function Footer() {
  const year = new Date().getFullYear()

  return (
    <footer className="marketing-footer">
      <div className="marketing-frame marketing-footer__inner">
        <p className="marketing-footer__statement">
          A calmer way to run software on infrastructure you control.
        </p>

        <div className="marketing-footer__meta">
          <PloyzLogo />
          <nav className="marketing-footer__links" aria-label="Footer">
            <Link to="/docs">Docs</Link>
            <Link to="/pricing">Pricing</Link>
            <a href="https://github.com/getployz/ployz">GitHub</a>
          </nav>
          <span>&copy; {year} Ployz</span>
        </div>
      </div>
    </footer>
  )
}
