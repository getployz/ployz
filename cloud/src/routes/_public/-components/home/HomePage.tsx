import { BigCTA } from '#/routes/_public/-components/home/BigCTA'
import { Compare } from '#/routes/_public/-components/home/Compare'
import { Efficiency } from '#/routes/_public/-components/home/Efficiency'
import { FeaturesJourney } from '#/routes/_public/-components/home/FeaturesJourney'
import { Hero } from '#/routes/_public/-components/home/Hero'
import { LogosRow } from '#/routes/_public/-components/home/LogosRow'
import { Pillars } from '#/routes/_public/-components/home/Pillars'
import { SectionIntro } from '#/routes/_public/-components/home/SectionIntro'

export function HomePage() {
  return (
    <main className="marketing-page">
      <Hero />
      <LogosRow />
      <SectionIntro />
      <FeaturesJourney />
      <Pillars />
      <Efficiency />
      <Compare />
      <BigCTA />
    </main>
  )
}
