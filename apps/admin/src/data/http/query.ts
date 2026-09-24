export type QueryValue = string | number | boolean | null | undefined
export type QueryValues = QueryValue | readonly QueryValue[]
export type QueryParameters = Readonly<Record<string, QueryValues>>

/**
 * Build a deterministic query string without accepting an already encoded URL.
 * Arrays are represented as repeated keys, which keeps this helper independent
 * from a server-side wire DTO convention.
 */
export function buildQueryString(parameters?: QueryParameters): string {
  if (!parameters) {
    return ''
  }

  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(parameters)) {
    if (value === undefined || value === null) {
      continue
    }
    const values = Array.isArray(value) ? value : [value]
    for (const item of values) {
      if (item !== undefined && item !== null) {
        search.append(key, String(item))
      }
    }
  }

  const encoded = search.toString()
  return encoded.length > 0 ? `?${encoded}` : ''
}

export function withQuery(path: string, parameters?: QueryParameters): string {
  const query = buildQueryString(parameters)
  if (!query) {
    return path
  }
  const separator = path.includes('?') ? '&' : '?'
  return `${path}${separator}${query.slice(1)}`
}
