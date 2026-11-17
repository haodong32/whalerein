// Comment/key-preserving field edits.
//
// smol-toml owns TOML semantics, but it does not retain source ranges. The
// maintained toml-eslint-parser AST supplies exact value/statement ranges, so
// patches replace only the affected value tokens and insert only new lines.
// Comments and unrelated keys outside those ranges remain byte-identical.
import { parse as parseTomlValue, stringify as stringifyToml } from 'smol-toml'
import { parseForESLint } from 'toml-eslint-parser'
import { isBuiltinEnvironment } from './catalog.js'
import { IDENTIFIER_PATTERN, normalizeTls } from './document.js'
import { configError } from './errors.js'

function decodeKeyPart(part, source) {
  if (part?.type === 'TOMLBare' && typeof part.name === 'string') return part.name
  const raw = source.slice(part.range[0], part.range[1])
  try {
    const parsed = parseTomlValue(`${raw} = 0`)
    return Object.keys(parsed)[0]
  } catch {
    throw configError('unknown_field', 'configuration contains an unreadable key')
  }
}

function keySegments(keyNode, source) {
  if (!keyNode || !Array.isArray(keyNode.keys)) return []
  return keyNode.keys.map((part) => decodeKeyPart(part, source))
}

function scan(text) {
  let ast
  try {
    ast = parseForESLint(text).ast
  } catch {
    throw configError('invalid_syntax', 'configuration could not be inspected')
  }
  const top = ast.body[0]
  const topKvs = []
  const tables = []
  for (const statement of top?.body ?? []) {
    if (statement.type === 'TOMLKeyValue') {
      topKvs.push({ path: keySegments(statement.key, text), node: statement, keyNode: statement.key })
    } else if (statement.type === 'TOMLTable') {
      const path = keySegments(statement.key, text)
      const kvs = []
      for (const child of statement.body ?? []) {
        if (child.type === 'TOMLKeyValue') {
          kvs.push({ path: keySegments(child.key, text), node: child, keyNode: child.key })
        }
      }
      tables.push({ path, node: statement, keyNode: statement.key, kvs })
    }
  }
  return { topKvs, tables }
}

function lineStart(text, offset) {
  const index = text.lastIndexOf('\n', offset - 1)
  return index === -1 ? 0 : index + 1
}

function afterLineEnd(text, offset) {
  const index = text.indexOf('\n', offset)
  return index === -1 ? text.length : index + 1
}

function removeLine(text, node) {
  return { start: lineStart(text, node.range[0]), end: afterLineEnd(text, node.range[1]), text: '' }
}

function insertion(text, offset, lines) {
  const leading = offset > 0 && text[offset - 1] !== '\n' ? '\n' : ''
  return { start: offset, end: offset, text: `${leading}${lines.join('\n')}\n` }
}

function serializeValue(value) {
  const line = stringifyToml({ value })
  const index = line.indexOf('=')
  return line.slice(index + 1).trim()
}

function applyEdits(text, edits) {
  const sorted = edits.filter(Boolean).sort((left, right) => right.start - left.start || right.end - left.end)
  let result = text
  for (const edit of sorted) {
    result = result.slice(0, edit.start) + edit.text + result.slice(edit.end)
  }
  return result
}

function findTable(scanResult, path) {
  return scanResult.tables.find((table) => table.path.length === path.length && table.path.every((part, index) => part === path[index])) ?? null
}

function findTopKeyValue(scanResult, path) {
  return scanResult.topKvs.find((kv) => kv.path.length === path.length && kv.path.every((part, i) => part === path[i])) ?? null
}

function tableInsertionOffset(text, table) {
  if (table.kvs.length > 0) return afterLineEnd(text, table.kvs[table.kvs.length - 1].node.range[1])
  return afterLineEnd(text, table.keyNode.range[1])
}

function fieldEdits(text, table, desired, order) {
  const edits = []
  const additions = []
  const existing = new Map(table.kvs.map((kv) => [kv.path.join('\u0000'), kv]))
  for (const field of order) {
    if (!Object.hasOwn(desired, field)) continue
    const value = desired[field]
    const current = existing.get(field)
    if (value === null) {
      if (current) edits.push(removeLine(text, current.node))
    } else if (value !== undefined) {
      if (current) {
        edits.push({ start: current.node.value.range[0], end: current.node.value.range[1], text: serializeValue(value) })
      } else {
        additions.push(`${field} = ${serializeValue(value)}`)
      }
    }
  }
  if (additions.length > 0) edits.push(insertion(text, tableInsertionOffset(text, table), additions))
  return edits
}

function appendSections(text, sections) {
  let result = text
  for (const lines of sections) {
    if (result.length > 0 && !result.endsWith('\n')) result += '\n'
    if (result.length > 0) result += '\n'
    result += `${lines.join('\n')}\n`
  }
  return result
}

function assertName(name) {
  if (typeof name !== 'string' || !IDENTIFIER_PATTERN.test(name)) {
    throw configError('invalid_identifier', `environment name must match ${IDENTIFIER_PATTERN}`)
  }
  return name
}

function tlsLines(name, tls) {
  const lines = [`[environments.${name}.tls]`, `ca_provider = ${serializeValue(tls.ca_provider)}`]
  if (tls.ca_file !== null) lines.push(`ca_file = ${serializeValue(tls.ca_file)}`)
  return lines
}

function desiredEnvironmentFields(definition) {
  const desired = { label: null, mode: definition.mode }
  if (definition.label !== undefined) {
    if (definition.label !== null && typeof definition.label !== 'string') {
      throw configError('unknown_field', 'environment label must be a string')
    }
    desired.label = definition.label
  }
  if (definition.mode === 'standalone') {
    desired.api_origin = null
    desired.issuer = null
    desired.native_client_id = null
  } else {
    for (const field of ['api_origin', 'issuer', 'native_client_id']) {
      desired[field] = definition[field] === undefined ? null : definition[field]
    }
  }
  return desired
}

function setDefaultEnvironment(text, scanResult, document, value) {
  const edits = []
  let desired = value
  if (desired !== undefined && desired !== null) {
    desired = assertName(desired)
    if (!(document.environments[desired] || isBuiltinEnvironment(desired))) {
      throw configError('unknown_environment', `environment ${desired} is not defined`, { environment: desired })
    }
  } else {
    desired = null
  }
  const current = findTopKeyValue(scanResult, ['environment'])
  if (current) {
    if (desired === null) edits.push(removeLine(text, current.node))
    else edits.push({ start: current.node.value.range[0], end: current.node.value.range[1], text: serializeValue(desired) })
  } else if (desired !== null) {
    const version = findTopKeyValue(scanResult, ['version'])
    const offset = version ? afterLineEnd(text, version.node.range[1]) : 0
    edits.push(insertion(text, offset, [`environment = ${serializeValue(desired)}`]))
  }
  return applyEdits(text, edits)
}

function upsertEnvironment(text, scanResult, name, definition) {
  assertName(name)
  if (isBuiltinEnvironment(name)) {
    throw configError('authority_override', `built-in environment ${name} cannot be replaced; use a new environment name`, { environment: name })
  }
  if (!definition || typeof definition !== 'object' || Array.isArray(definition)) {
    throw configError('unknown_field', 'environment definition must be a table')
  }
  const mode = definition.mode
  if (mode !== 'account' && mode !== 'standalone') {
    throw configError('invalid_mode', `environment ${name} must declare mode account or standalone`, { environment: name })
  }
  if (mode === 'standalone') {
    for (const field of ['api_origin', 'issuer', 'native_client_id']) {
      if (definition[field] !== undefined) {
        throw configError('invalid_mode_fields', `standalone environment ${name} cannot set ${field}`, { environment: name })
      }
    }
  } else {
    for (const field of ['api_origin', 'issuer', 'native_client_id']) {
      if (typeof definition[field] !== 'string' || definition[field].length === 0) {
        throw configError('invalid_mode_fields', `account environment ${name} requires a non-empty ${field}`, { environment: name })
      }
    }
  }
  let tls = null
  if (definition.tls !== undefined && definition.tls !== null) {
    tls = typeof definition.tls === 'string'
      ? normalizeTls(name, { ca_provider: definition.tls })
      : normalizeTls(name, definition.tls)
  }
  const desired = desiredEnvironmentFields(definition)
  const table = findTable(scanResult, ['environments', name])
  let result = text
  const edits = []
  if (table) {
    edits.push(...fieldEdits(text, table, desired, ['label', 'mode', 'api_origin', 'issuer', 'native_client_id']))
  }
  const tlsTable = findTable(scanResult, ['environments', name, 'tls'])
  if (tlsTable) {
    if (tls === null) edits.push(removeLine(text, tlsTable.node))
    else edits.push(...fieldEdits(text, tlsTable, { ca_provider: tls.ca_provider, ca_file: tls.ca_file }, ['ca_provider', 'ca_file']))
  }
  if (!table) {
    const lines = [`[environments.${name}]`]
    for (const field of ['label', 'mode', 'api_origin', 'issuer', 'native_client_id']) {
      if (desired[field] !== null && desired[field] !== undefined) lines.push(`${field} = ${serializeValue(desired[field])}`)
    }
    result = appendSections(result, [lines])
  } else {
    result = applyEdits(result, edits)
  }
  if (tls !== null && (!table || !tlsTable)) {
    result = appendSections(result, [tlsLines(name, tls)])
  }
  return result
}

function specializeBuiltin(text, scanResult, name, patch) {
  if (!isBuiltinEnvironment(name)) {
    throw configError('invalid_argument', 'only built-in environments accept label/trust specialization', { environment: name })
  }
  const allowed = new Set(['label', 'clear_label', 'tls', 'clear_tls', 'type', 'kind', 'name', 'environment'])
  for (const key of Object.keys(patch)) {
    if (!allowed.has(key)) {
      throw configError('authority_override', `built-in environment ${name} only allows label and tls`, { environment: name })
    }
  }
  const desired = {}
  if (patch.clear_label === true) desired.label = null
  else if (Object.hasOwn(patch, 'label')) {
    if (patch.label !== null && typeof patch.label !== 'string') throw configError('invalid_argument', 'environment label must be a string')
    desired.label = patch.label
  }
  let tls
  if (patch.clear_tls === true) tls = null
  else if (Object.hasOwn(patch, 'tls')) {
    if (patch.tls === null || patch.tls === undefined) tls = null
    else tls = typeof patch.tls === 'string'
      ? normalizeTls(name, { ca_provider: patch.tls })
      : normalizeTls(name, patch.tls)
  }
  const table = findTable(scanResult, ['environments', name])
  const tlsTable = findTable(scanResult, ['environments', name, 'tls'])
  let result = text
  const edits = []
  if (table) edits.push(...fieldEdits(text, table, desired, ['label']))
  if (tlsTable && tls === null) edits.push(removeLine(text, tlsTable.node))
  else if (tlsTable && tls !== undefined) edits.push(...fieldEdits(text, tlsTable, { ca_provider: tls.ca_provider, ca_file: tls.ca_file }, ['ca_provider', 'ca_file']))
  if (table) result = applyEdits(result, edits)
  else if (desired.label !== null && desired.label !== undefined) result = appendSections(result, [[`[environments.${name}]`, `label = ${serializeValue(desired.label)}`]])
  if (tls !== undefined && tls !== null && !tlsTable) result = appendSections(result, [tlsLines(name, tls)])
  return result
}

function removeEnvironment(text, scanResult, document, name) {
  assertName(name)
  if (isBuiltinEnvironment(name)) {
    throw configError('authority_override', `built-in environment ${name} cannot be removed`, { environment: name })
  }
  if (!document.environments[name]) {
    throw configError('unknown_environment', `environment ${name} is not defined`, { environment: name })
  }
  const edits = []
  for (const path of [['environments', name], ['environments', name, 'tls']]) {
    const table = findTable(scanResult, path)
    if (table) edits.push(removeLine(text, table.node))
  }
  return applyEdits(text, edits)
}

/**
 * Apply a typed settings patch to TOML text. The resulting text is always
 * revalidated by the caller before it can be written; an invalid patch never
 * reaches the filesystem.
 */
export function applyPatch(text, patch, document) {
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) {
    throw configError('invalid_argument', 'settings patch must be a table')
  }
  const raw = patch.type ?? patch.kind
  const aliases = {
    SetDefaultEnvironment: 'set_default_environment',
    set_default_environment: 'set_default_environment',
    UpsertCustomEnvironment: 'upsert_custom_environment',
    upsert_custom_environment: 'upsert_custom_environment',
    upsert_environment: 'upsert_custom_environment',
    SpecializeBuiltin: 'specialize_builtin',
    specialize_builtin: 'specialize_builtin',
    RemoveCustomEnvironment: 'remove_custom_environment',
    remove_custom_environment: 'remove_custom_environment',
    remove_environment: 'remove_custom_environment',
  }
  const type = aliases[raw]
  if (!type) throw configError('invalid_argument', 'unsupported settings patch type')
  const scanResult = scan(text)
  switch (type) {
    case 'set_default_environment':
      return setDefaultEnvironment(text, scanResult, document, patch.environment)
    case 'upsert_custom_environment': {
      const name = patch.name ?? patch.environment
      assertName(name)
      const definition = patch.definition && typeof patch.definition === 'object'
        ? patch.definition
        : {
            mode: patch.mode,
            label: patch.label,
            api_origin: patch.api_origin,
            issuer: patch.issuer,
            native_client_id: patch.native_client_id,
            tls: patch.tls,
          }
      return upsertEnvironment(text, scanResult, name, definition)
    }
    case 'specialize_builtin': {
      const name = patch.name ?? patch.environment
      assertName(name)
      return specializeBuiltin(text, scanResult, name, patch)
    }
    case 'remove_custom_environment': {
      const name = patch.name ?? patch.environment
      assertName(name)
      return removeEnvironment(text, scanResult, document, name)
    }
    default:
      throw configError('invalid_argument', 'unsupported settings patch type')
  }
}

export { serializeValue }
