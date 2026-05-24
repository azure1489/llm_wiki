import { getFileName, normalizePath } from "@/lib/path-utils"

const RAW_SOURCES_PREFIX = "raw/sources/"
const RAW_SOURCES_MARKER = "/raw/sources/"

export function sourceIdentityForPath(projectPath: string, sourcePath: string): string {
  const pp = normalizePath(projectPath).replace(/\/+$/, "")
  const sp = normalizePath(sourcePath)
  const projectRawSourcesPrefix = `${pp}/${RAW_SOURCES_PREFIX}`
  if (sp.startsWith(projectRawSourcesPrefix)) {
    return sp.slice(projectRawSourcesPrefix.length)
  }
  if (sp.startsWith(RAW_SOURCES_PREFIX)) {
    return sp.slice(RAW_SOURCES_PREFIX.length)
  }
  if (sp.includes(RAW_SOURCES_MARKER)) {
    return sp.slice(sp.indexOf(RAW_SOURCES_MARKER) + RAW_SOURCES_MARKER.length)
  }
  return getFileName(sp)
}

export function sourceReferenceIdentity(sourceReference: string): string {
  const ref = normalizePath(sourceReference)
  if (ref.startsWith(RAW_SOURCES_PREFIX)) {
    return ref.slice(RAW_SOURCES_PREFIX.length)
  }
  if (ref.includes(RAW_SOURCES_MARKER)) {
    return ref.slice(ref.indexOf(RAW_SOURCES_MARKER) + RAW_SOURCES_MARKER.length)
  }
  return ref
}

export function sourceSummarySlugFromIdentity(sourceIdentity: string): string {
  const withoutExt = sourceIdentity.replace(/\.[^/.]+$/, "")
  const parts = withoutExt
    .split("/")
    .map((part) => part.trim())
    .filter(Boolean)

  if (parts.length <= 1) {
    return parts[0] || "source"
  }

  const slug = parts.map((part) => {
    const encoded = encodeURIComponent(part).replace(
      /[!'()*]/g,
      (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`,
    )
    return `${encoded.length}-${encoded}`
  }).join("--")
  return `${slug}--${stableSlugHash(sourceIdentity)}`
}

function stableSlugHash(value: string): string {
  let hash = 0x811c9dc5
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193)
  }
  return (hash >>> 0).toString(36)
}
