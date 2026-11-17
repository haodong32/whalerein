#!/usr/bin/env node
// Copy the canonical client-environment contract into the Node package.
//
// The canonical files live in `contracts/client-environment/` and are the only
// editable sources. Generated copies are shipped so the installed package never
// reads a repository-relative path. Run this after changing the canonical
// contract; a test fails when the copies drift.
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const runtimeRoot = join(here, '..')
const canonical = join(runtimeRoot, '..', '..', 'contracts', 'client-environment')
const target = join(runtimeRoot, 'src', 'configuration')

const files = ['catalog.json', 'cases.json']
mkdirSync(target, { recursive: true })
for (const file of files) copyFileSync(join(canonical, file), join(target, file))
process.stdout.write(`synced ${files.length} contract files to ${target}\n`)
