import { createReadStream, statSync } from 'node:fs'
import { join } from 'node:path'

// /grader/<sha256>.wasm: the enclave grader builds, from a folder on the
// server (never in git). OutLayer checks the SHA-256 of what it downloads
// against the build registered on the contract, so serving the file here is
// as safe as any other host and costs no gas. Only names that are a hash are
// served.
const DIR = process.env.SPHINX_GRADER_DIR || '/grader'

export default defineEventHandler((event) => {
  const name = getRouterParam(event, 'name') ?? ''
  if (!/^[0-9a-f]{64}\.wasm$/.test(name)) throw createError({ statusCode: 404 })
  const path = join(DIR, name)
  let size: number
  try {
    size = statSync(path).size
  } catch {
    throw createError({ statusCode: 404 })
  }
  setResponseHeaders(event, {
    'content-type': 'application/wasm',
    'content-length': size,
    'cache-control': 'public, max-age=31536000, immutable'
  })
  return sendStream(event, createReadStream(path))
})
