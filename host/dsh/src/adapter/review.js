// Bounded, credential-masked review facts for one live native approval.
//
// This module is deliberately pure: it reads a native approval request once,
// derives at most three short facts, and drops the request. The raw argument
// map is never returned, stored, logged or traced. Callers may keep only the
// four fields below, which are bounded to REVIEW_FIELD_MAX code points and
// stripped of control characters.
//
// Extraction is conservative:
//   * a fixed allowlist of known DSH file/shell tools, or an explicit
//     operation intent in the argument map, selects the action;
//   * an unknown tool name never yields an action or target;
//   * a missing essential fact is reported as incomplete/unavailable instead
//     of inventing a plausible-looking value.
// Tool names are only matched exactly against the allowlist above, never
// searched heuristically, and free text is never used to infer impact.
//
// These four fields are advisory display data, not an authorization gate. A
// live native approval may legitimately expose no argument map, so callers
// resolve the decision against the bound digest/version instead of requiring a
// complete review. A client may still choose to withhold Allow in its own UI.

import {
  REVIEW_FIELD_MAX,
  REVIEW_REDACTED,
  boundReviewValue,
  maskReviewValue,
} from 'whalerein-host/security/redaction.js'

export { REVIEW_FIELD_MAX, REVIEW_REDACTED, boundReviewValue, maskReviewValue }

/** Normalize a proto name, token or numeric value to the canonical token. */
export function normalizeReviewAvailability(value) {
  if (value === 'complete' || value === 'approval_review_complete'
    || value === 'APPROVAL_REVIEW_COMPLETE' || value === 1) return 'complete'
  if (value === 'incomplete' || value === 'approval_review_incomplete'
    || value === 'APPROVAL_REVIEW_INCOMPLETE' || value === 2) return 'incomplete'
  return 'unavailable'
}

// Exact DSH tool identifiers. Substring or prefix matching is never used: a
// tool named `bash_helper` or `write_anything` is unknown, not a bash/write.
const FILE_ACTIONS = Object.freeze({
  read: 'Read file',
  read_image: 'Read image',
  write: 'Write file',
  edit: 'Edit file',
  apply_patch: 'Apply patch',
})

// Explicit operation intents accepted from the native argument map. These are
// the operation verbs, not a tool-name guess.
const OPERATION_ACTIONS = Object.freeze({
  read: 'Read file',
  write: 'Write file',
  create: 'Create file',
  edit: 'Edit file',
  delete: 'Delete file',
  patch: 'Patch file',
  move: 'Move file',
  copy: 'Copy file',
  apply_patch: 'Apply patch',
})

const COMMAND_TOOLS = new Set(['bash', 'pwsh', 'shell', 'sh', 'zsh', 'powershell', 'cmd', 'exec'])

const SANDBOX_MODES = new Set(['read-only', 'workspace-write', 'danger-full-access'])

// Ordered path-like argument names. Only one value is read, and only a bounded
// string that still looks like a path. File contents under these keys are
// bounded and masked like everything else; they are never forwarded.
const PATH_KEYS = Object.freeze([
  'path', 'file_path', 'filePath', 'filepath', 'file',
  'target_path', 'targetPath', 'target', 'destination', 'dest',
  'new_path', 'newPath', 'to', 'old_path', 'oldPath', 'from', 'source',
])

const MOVE_PATH_KEYS = Object.freeze(['destination', 'dest', 'new_path', 'newPath', 'to', 'target', 'target_path', 'targetPath'])

/**
 * Read one candidate argument map from the native request without retaining
 * it. DSH 0.2.2-rc.1 exposes no argument map on the approval event; a request
 * that carries one (fixtures or a newer native API) is accepted here.
 */
function requestArguments(request) {
  for (const key of ['arguments', 'args', 'input', 'parameters', 'toolArguments']) {
    const raw = request?.[key]
    if (typeof raw === 'string' && raw.trim() !== '') {
      try {
        const parsed = JSON.parse(raw)
        if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) return parsed
      } catch {
        // Not a JSON object: ignore rather than parse free text heuristically.
      }
      continue
    }
    if (raw && typeof raw === 'object' && !Array.isArray(raw)) return raw
  }
  return null
}

/** A scalar argument, limited to strings and finite numbers. */
function scalar(args, key) {
  if (!args || typeof args !== 'object') return undefined
  const value = args[key]
  if (typeof value === 'string') return value
  if (typeof value === 'number' && Number.isFinite(value)) return value
  return undefined
}

/** One bounded path-like string, or '' when nothing safe is available. */
function pathValue(args, keys = PATH_KEYS) {
  for (const key of keys) {
    const value = scalar(args, key)
    if (typeof value !== 'string') continue
    const candidate = value.trim()
    if (candidate === '') continue
    // A path-like value is single-line, bounded and not an inline object.
    if (/[\u0000-\u001F\u007F]/.test(candidate)) continue
    if (candidate.startsWith('-')) continue
    if (/^[[{]/.test(candidate)) continue
    if (candidate.length > 512) continue
    return candidate
  }
  return ''
}

/** Number of lines in a fragment without allocating a split array. */
function lineCount(value) {
  if (typeof value !== 'string' || value === '') return 0
  let count = 1
  const limit = Math.min(value.length, 1_000_000)
  for (let index = 0; index < limit; index++) if (value.charCodeAt(index) === 10) count += 1
  return count
}

function byteCount(value) {
  return typeof value === 'string' ? Buffer.byteLength(value, 'utf8') : 0
}

/** Fixed impact phrase for a file operation. Only counts leave this scope. */
function fileImpact(kind, args) {
  switch (kind) {
    case 'read': {
      const offset = scalar(args, 'offset')
      const limit = scalar(args, 'limit')
      if (typeof offset === 'number' && typeof limit === 'number' && offset >= 0 && limit > 0) {
        return `Lines ${offset} to ${offset + limit - 1}`
      }
      return 'Whole file'
    }
    case 'read_image':
      return 'Whole image'
    case 'write': {
      const content = scalar(args, 'content') ?? scalar(args, 'contents') ?? scalar(args, 'text')
      if (typeof content === 'string') {
        const lines = lineCount(content)
        return `Whole file, ${lines} line${lines === 1 ? '' : 's'}, ${byteCount(content)} bytes`
      }
      return 'Whole file'
    }
    case 'edit': {
      const edits = args?.edits
      if (Array.isArray(edits) && edits.length > 0) {
        return edits.length === 1 ? '1 edit' : `${edits.length} edits`
      }
      const before = scalar(args, 'old_string') ?? scalar(args, 'oldText')
      const after = scalar(args, 'new_string') ?? scalar(args, 'newText')
      const lines = Math.max(
        typeof before === 'string' ? lineCount(before) : 0,
        typeof after === 'string' ? lineCount(after) : 0,
      )
      return lines > 0 ? `${lines} line${lines === 1 ? '' : 's'} changed` : 'Whole file'
    }
    case 'patch':
    case 'apply_patch': {
      const patch = scalar(args, 'patch') ?? scalar(args, 'diff') ?? scalar(args, 'input')
      if (typeof patch === 'string' && patch !== '') {
        const hunks = (patch.match(/^@@/gm) ?? []).length
        const files = (patch.match(/^\*\*\* (?:Add|Update|Delete) File:/gm) ?? []).length
        if (hunks > 0 || files > 0) {
          const hunkText = hunks === 1 ? '1 hunk' : `${hunks} hunks`
          const fileText = files === 1 ? '1 file' : `${files} files`
          return `Apply ${hunkText} across ${fileText}`
        }
      }
      return 'Whole file'
    }
    default:
      return 'Whole file'
  }
}

/** First path named by a structured patch header, or ''. */
function patchTarget(args) {
  const patch = scalar(args, 'patch') ?? scalar(args, 'diff') ?? scalar(args, 'input')
  if (typeof patch !== 'string' || patch === '') return ''
  const match = /^\*\*\* (?:Add|Update|Delete) File: (.+)$/m.exec(patch)
  if (!match) return ''
  const candidate = match[1].trim()
  if (candidate === '' || candidate.length > 512 || /[\u0000-\u001F\u007F]/.test(candidate)) return ''
  if (candidate.startsWith('-')) return ''
  return candidate
}

/** Executable basename from the first non-assignment token, or ''. */
function commandTarget(args) {
  const command = scalar(args, 'command') ?? scalar(args, 'cmd') ?? scalar(args, 'script')
  if (typeof command !== 'string' || command === '') return ''
  let rest = command.trim()
  while (rest !== '') {
    const token = /^\S+/.exec(rest)?.[0] ?? ''
    if (token === '') return ''
    rest = rest.slice(token.length).trimStart()
    // Skip leading environment assignments without keeping their values.
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(token)) continue
    if (token === 'env' || token === 'command' || token === 'exec') continue
    const base = token.split(/[\\/]/).pop() ?? ''
    // Only a plain executable name is safe: no shell operators, variables,
    // globs, quotes or argument text.
    if (!/^[A-Za-z0-9._+-]{1,64}$/.test(base)) return ''
    return base
  }
  return ''
}

function commandImpact(args) {
  const mode = scalar(args, 'sandbox_permissions')
  if (typeof mode === 'string' && SANDBOX_MODES.has(mode)) return `One shell command, ${mode} access`
  return 'One shell command'
}

/** Compute the availability token from the established fields. */
export function reviewAvailabilityFor(action, target, impact) {
  const established = [action, target, impact].filter(Boolean).length
  return established === 3 ? 'complete' : established === 0 ? 'unavailable' : 'incomplete'
}

/**
 * Derive the four review fields from one live native approval request.
 *
 * @param {object|null|undefined} request native request with `toolName`,
 *   optional `callId`/`reason`, and optionally a structured argument map.
 * @returns {{reviewAction: string, reviewTarget: string, reviewImpact: string,
 *   reviewAvailability: 'complete'|'incomplete'|'unavailable'}}
 */
export function describeApprovalRequest(request) {
  const empty = { reviewAction: '', reviewTarget: '', reviewImpact: '', reviewAvailability: 'unavailable' }
  if (!request || typeof request !== 'object') return empty
  const toolName = typeof request.toolName === 'string' ? request.toolName.trim().toLowerCase() : ''
  const args = requestArguments(request)
  const intent = typeof scalar(args, 'operation') === 'string' ? scalar(args, 'operation').trim().toLowerCase() : ''

  let action = ''
  let kind = ''
  if (intent !== '' && OPERATION_ACTIONS[intent]) {
    action = OPERATION_ACTIONS[intent]
    kind = intent
  } else if (toolName !== '' && FILE_ACTIONS[toolName]) {
    action = FILE_ACTIONS[toolName]
    kind = toolName === 'apply_patch' ? 'apply_patch' : toolName
  } else if (toolName !== '' && COMMAND_TOOLS.has(toolName)) {
    action = 'Run command'
    kind = 'command'
  }
  // An unknown tool name never supplies an action, target or impact.

  let target = ''
  let impact = ''
  if (kind === 'command') {
    target = commandTarget(args)
    impact = commandImpact(args)
  } else if (kind !== '') {
    const keys = kind === 'move' || kind === 'copy' ? MOVE_PATH_KEYS : PATH_KEYS
    target = pathValue(args, keys)
    if (target === '' && (kind === 'patch' || kind === 'apply_patch')) target = patchTarget(args)
    impact = fileImpact(kind, args)
  }

  const maskedAction = boundReviewValue(action)
  const maskedTarget = boundReviewValue(target)
  const maskedImpact = boundReviewValue(impact)
  return {
    reviewAction: maskedAction,
    reviewTarget: maskedTarget,
    reviewImpact: maskedImpact,
    reviewAvailability: reviewAvailabilityFor(maskedAction, maskedTarget, maskedImpact),
  }
}