import { readdirSync } from 'node:fs'
import { extname, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'

const extensions = new Set(['.cjs', '.js', '.mjs'])

function collect(path) {
  const entries = readdirSync(path, { withFileTypes: true })
  return entries.flatMap((entry) => {
    const child = resolve(path, entry.name)
    if (entry.isDirectory()) return collect(child)
    return entry.isFile() && extensions.has(extname(entry.name)) ? [child] : []
  })
}

const roots = process.argv.slice(2)
if (roots.length === 0) {
  process.stderr.write('Usage: node check-node-syntax.mjs <file-or-directory> [...\n')
  process.exit(2)
}

const files = roots.flatMap((root) => {
  const path = resolve(process.cwd(), root)
  try {
    return collect(path)
  } catch (error) {
    if (error.code === 'ENOTDIR') return [path]
    throw error
  }
}).sort()

for (const file of files) {
  const result = spawnSync(process.execPath, ['--check', file], { stdio: 'inherit' })
  if (result.error) throw result.error
  if (result.status !== 0) process.exit(result.status ?? 1)
}

process.stdout.write(`Checked syntax in ${files.length} JavaScript files.\n`)
