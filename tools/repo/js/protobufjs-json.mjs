#!/usr/bin/env node
// Generate the protobufjs static JSON descriptor packaged with whalerein-host.
//
// Usage:
//   node protobufjs-json.mjs <proto-file> <output-json> [<resolve-from-dir>]
//
// protobufjs is resolved from the Host package (its runtime dependency) so
// regeneration works offline without a global install.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'

const [protoPath, outPath, resolveFrom = process.cwd()] = process.argv.slice(2)
if (!protoPath || !outPath) {
  console.error('usage: protobufjs-json.mjs <proto-file> <output-json> [<resolve-from-dir>]')
  process.exit(2)
}

const require = createRequire(resolve(resolveFrom, 'package.json'))
let protobuf
try {
  protobuf = require('protobufjs')
} catch (error) {
  console.error(`cannot load protobufjs from ${resolveFrom}: ${error.message}`)
  process.exit(3)
}

const source = readFileSync(protoPath, 'utf8')
const parsed = protobuf.parse(source, { keepCase: true })
const json = parsed.root.toJSON({ keepComments: false })

mkdirSync(dirname(outPath), { recursive: true })
writeFileSync(outPath, JSON.stringify(json, null, 2) + '\n')
console.log(`wrote ${outPath}`)
