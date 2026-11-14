import { readFile } from 'node:fs/promises'

// The daily drill's newest runs, as the drill service wrote them; an empty
// list when there is no drill on this server.
export default defineEventHandler(async () => {
  const file = useRuntimeConfig().drillFile
  if (!file) return { runs: [] }
  try {
    const doc = JSON.parse(await readFile(file, 'utf8')) as { runs?: unknown[] }
    return { runs: Array.isArray(doc.runs) ? doc.runs : [] }
  } catch {
    return { runs: [] }
  }
})
