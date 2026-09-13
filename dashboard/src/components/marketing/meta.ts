type MarketingMetaOptions = {
  title: string
  description: string
}

export function buildMarketingMeta({
  title,
  description,
}: MarketingMetaOptions) {
  return [
    { title },
    { name: 'description', content: description },
    { property: 'og:type', content: 'website' },
    { property: 'og:title', content: title },
    { property: 'og:description', content: description },
    { name: 'twitter:card', content: 'summary' },
    { name: 'twitter:title', content: title },
    { name: 'twitter:description', content: description },
  ]
}
