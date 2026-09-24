const RELEASE_VERSION =
  /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/

/** A portable, path-safe SemVer release identifier (without a leading v). */
export function isReleaseVersion(value) {
  if (typeof value !== 'string') return false
  const match = RELEASE_VERSION.exec(value)
  if (!match) return false
  const prerelease = match[1]
  return (
    prerelease === undefined ||
    prerelease
      .split('.')
      .every(
        (identifier) =>
          !/^[0-9]+$/.test(identifier) || identifier === '0' || !identifier.startsWith('0'),
      )
  )
}
