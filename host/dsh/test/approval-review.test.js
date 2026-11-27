// AC-47: bounded, credential-masked approval review facts. The helper reads a
// live native request once and returns only four bounded fields; raw arguments
// must never survive, and unknown context must never be guessed.

import test from 'node:test'
import assert from 'node:assert/strict'
import {
  REVIEW_FIELD_MAX,
  REVIEW_REDACTED,
  boundReviewValue,
  describeApprovalRequest,
  maskReviewValue,
  normalizeReviewAvailability,
  reviewAvailabilityFor,
} from '../src/adapter/review.js'
import { approvalDigest } from 'whalerein-host/security/digest.js'

function facts(request) {
  return describeApprovalRequest(request)
}

test('a file edit projects action, path and a bounded line extent', () => {
  const review = facts({
    toolName: 'edit',
    arguments: { path: '/repo/src/app.js', old_string: 'a\nb\nc', new_string: 'a\nB\nc' },
  })
  assert.deepEqual(review, {
    reviewAction: 'Edit file',
    reviewTarget: '/repo/src/app.js',
    reviewImpact: '3 lines changed',
    reviewAvailability: 'complete',
  })
})

test('a file write reports whole-file line and byte extent without content', () => {
  const content = 'one\ntwo'
  const review = facts({ toolName: 'write', arguments: { file_path: '/repo/notes.txt', content } })
  assert.equal(review.reviewAction, 'Write file')
  assert.equal(review.reviewTarget, '/repo/notes.txt')
  assert.equal(review.reviewImpact, 'Whole file, 2 lines, 7 bytes')
  assert.equal(review.reviewAvailability, 'complete')
  assert.equal(JSON.stringify(review).includes(content), false)
})

test('apply_patch uses the structured patch header and hunks, never the diff body', () => {
  const patch = [
    '*** Begin Patch',
    '*** Update File: src/a.js',
    '@@',
    '-const token = "super-secret-token-value"',
    '+const token = redact()',
    '*** Update File: src/b.js',
    '@@',
    '-old',
    '+new',
    '*** End Patch',
  ].join('\n')
  const review = facts({ toolName: 'apply_patch', arguments: { patch } })
  assert.equal(review.reviewAction, 'Apply patch')
  assert.equal(review.reviewTarget, 'src/a.js')
  assert.equal(review.reviewImpact, 'Apply 2 hunks across 2 files')
  assert.equal(review.reviewAvailability, 'complete')
  assert.equal(JSON.stringify(review).includes('super-secret-token-value'), false)
})

test('an explicit operation verb selects create/delete/move/copy intent', () => {
  const created = facts({ toolName: 'workspace_fs', arguments: { operation: 'create', path: '/repo/new.md' } })
  assert.equal(created.reviewAction, 'Create file')
  assert.equal(created.reviewTarget, '/repo/new.md')
  const moved = facts({ toolName: 'workspace_fs', arguments: { operation: 'move', from: '/repo/a', to: '/repo/b' } })
  assert.equal(moved.reviewAction, 'Move file')
  assert.equal(moved.reviewTarget, '/repo/b')
  assert.equal(moved.reviewAvailability, 'complete')
})

test('a shell command exposes only the executable basename and a generic impact', () => {
  const review = facts({
    toolName: 'bash',
    arguments: { command: 'FOO=bar /usr/local/bin/npm run test --token=plain-secret-value' },
  })
  assert.equal(review.reviewAction, 'Run command')
  assert.equal(review.reviewTarget, 'npm')
  assert.equal(review.reviewImpact, 'One shell command')
  assert.equal(review.reviewAvailability, 'complete')
  assert.equal(JSON.stringify(review).includes('plain-secret-value'), false)
  assert.equal(JSON.stringify(review).includes('/usr/local/bin'), false)
  assert.equal(JSON.stringify(review).includes('--token'), false)
})

test('an escalated command reports the structured sandbox mode only', () => {
  const review = facts({
    toolName: 'bash',
    arguments: { command: 'npm test', sandbox_permissions: 'danger-full-access' },
  })
  assert.equal(review.reviewImpact, 'One shell command, danger-full-access access')
  assert.equal(review.reviewAvailability, 'complete')
})

test('credential-looking values in the target are replaced by a fixed placeholder', () => {
  const cases = [
    ['/tmp/sk-live-abcdefghijklmnop/config.json', '/tmp/<redacted>/config.json'],
    ['/tmp/0123456789abcdef0123456789abcdef/key', '/tmp/<redacted>/key'],
    ['/tmp/file?token=plain-secret-value', '/tmp/file?token=<redacted>'],
    ['https://user:pass@example.test/x', 'https://<redacted>@example.test/x'],
  ]
  for (const [path, expected] of cases) {
    const review = facts({ toolName: 'read', arguments: { path } })
    assert.equal(review.reviewTarget, expected, path)
    assert.equal(review.reviewAvailability, 'complete')
  }
})

test('masking covers bearer tokens, key assignments, JWTs, PEM keys and long hex', () => {
  const secret = 'sk-live-abcdefghijklmnop'
  const bearer = 'Bearer abcdefghijklmnopqrstuvwxyz012345'
  const assignment = 'password=hunter2'
  const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U'
  const pem = [
    '-----BEGIN ',
    'PRIVATE KEY-----\nMIIEvQIBADANBgkq\n-----END ',
    'PRIVATE KEY-----',
  ].join('')
  const hex = 'a'.repeat(64)
  for (const value of [secret, bearer, assignment, jwt, pem, hex]) {
    const masked = maskReviewValue(value)
    assert.equal(masked.includes(value), false, value)
    assert.ok(masked.includes(REVIEW_REDACTED), value)
  }
  assert.equal(boundReviewValue('ok\u0000value\nnext'), 'ok value next')
})

test('missing arguments never invent a target and report incomplete', () => {
  const review = facts({ toolName: 'write', reason: 'needs wider access' })
  assert.equal(review.reviewAction, 'Write file')
  assert.equal(review.reviewTarget, '')
  assert.equal(review.reviewImpact, 'Whole file')
  assert.equal(review.reviewAvailability, 'incomplete')
  assert.equal(JSON.stringify(review).includes('reason'), false)
})

test('unknown tool names never derive action, target or impact', () => {
  for (const toolName of ['super_exfiltrate', 'bash_helper', 'write_anything', 'trash']) {
    const review = facts({ toolName, arguments: { path: '/etc/passwd', command: 'curl evil' } })
    assert.deepEqual(review, {
      reviewAction: '',
      reviewTarget: '',
      reviewImpact: '',
      reviewAvailability: 'unavailable',
    }, toolName)
  }
  assert.deepEqual(facts(null), {
    reviewAction: '', reviewTarget: '', reviewImpact: '', reviewAvailability: 'unavailable',
  })
})

test('review fields are bounded below the wire limit and control characters stripped', () => {
  const path = `/${Array.from({ length: 40 }, () => 'segment').join('/')}`
  const review = facts({ toolName: 'edit', arguments: { path, old_string: 'a', new_string: 'b' } })
  assert.ok(review.reviewTarget.length > 0)
  assert.ok(Array.from(review.reviewTarget).length <= REVIEW_FIELD_MAX)
  assert.equal(/[\u0000-\u001F\u007F]/.test(review.reviewTarget), false)
})

test('a changed target, impact or availability changes the v2 digest', () => {
  const base = {
    approvalId: 'a1', sessionId: 's1', toolName: 'edit', callId: 'c1', reason: 'r',
    version: 2, expiresAt: 99,
  }
  const one = { ...base, reviewAction: 'Edit file', reviewTarget: '/repo/a', reviewImpact: '1 line changed', reviewAvailability: 'complete' }
  const otherTarget = { ...one, reviewTarget: '/repo/b' }
  const otherImpact = { ...one, reviewImpact: '2 lines changed' }
  const otherAvailability = { ...one, reviewAvailability: 'incomplete' }
  const digests = [one, otherTarget, otherImpact, otherAvailability].map((approval) => [...approvalDigest(approval)].join(','))
  assert.equal(new Set(digests).size, 4)
})

test('availability normalization and derivation accept proto names and numerics', () => {
  assert.equal(normalizeReviewAvailability('complete'), 'complete')
  assert.equal(normalizeReviewAvailability('approval_review_incomplete'), 'incomplete')
  assert.equal(normalizeReviewAvailability(1), 'complete')
  assert.equal(normalizeReviewAvailability(2), 'incomplete')
  assert.equal(normalizeReviewAvailability('approval_review_unavailable'), 'unavailable')
  assert.equal(normalizeReviewAvailability(undefined), 'unavailable')
  assert.equal(reviewAvailabilityFor('a', 'b', 'c'), 'complete')
  assert.equal(reviewAvailabilityFor('a', '', 'c'), 'incomplete')
  assert.equal(reviewAvailabilityFor('', '', ''), 'unavailable')
})
test('approval normalization masks explicit review facts and canonicalizes availability', async () => {
  const { normalizeApproval } = await import('../src/adapter/base.js')
  const approval = normalizeApproval({
    approvalId: 'a', sessionId: 's', reviewAvailability: 'approval_review_incomplete',
    reviewAction: 'Edit file', reviewTarget: '/repo/sk-live-abcdefghijklmnop/app.js',
    reviewImpact: 'line changed '.repeat(40),
  })
  assert.equal(approval.reviewTarget, '/repo/<redacted>/app.js')
  assert.equal(approval.reviewAvailability, 'incomplete')
  assert.equal(Array.from(approval.reviewImpact).length, 256)
})

test('structured argument maps may arrive as a JSON string and fall back when unparsable', () => {
  const parsed = facts({
    toolName: 'write',
    arguments: JSON.stringify({ path: '/repo/a.txt', content: 'one\ntwo\nthree' }),
  })
  assert.equal(parsed.reviewTarget, '/repo/a.txt')
  assert.equal(parsed.reviewImpact, 'Whole file, 3 lines, 13 bytes')
  assert.equal(parsed.reviewAvailability, 'complete')

  const fallback = facts({
    toolName: 'write',
    arguments: 'not a json object',
    parameters: { filePath: '/repo/b.txt', contents: 'x' },
  })
  assert.equal(fallback.reviewTarget, '/repo/b.txt')
  assert.equal(fallback.reviewImpact, 'Whole file, 1 line, 1 bytes')
})

test('read and edit extent variants stay bounded and never include content', () => {
  const ranged = facts({ toolName: 'read', arguments: { path: '/repo/a', offset: 10, limit: 10 } })
  assert.equal(ranged.reviewImpact, 'Lines 10 to 19')
  const image = facts({ toolName: 'read_image', arguments: { file_path: '/repo/a.png' } })
  assert.equal(image.reviewImpact, 'Whole image')
  const edit = facts({ toolName: 'edit', arguments: { path: '/repo/a', edits: [{}, {}] } })
  assert.equal(edit.reviewImpact, '2 edits')
})

test('an unparsable command target is withheld instead of exposing shell text', () => {
  for (const command of ['$(curl evil)', '`rm -rf /`', 'VAR=secret $(evil)', 'FOO=1 env', '']) {
    const review = facts({ toolName: 'bash', arguments: { command } })
    assert.equal(review.reviewTarget, '', command)
    assert.equal(review.reviewAvailability, 'incomplete', command)
  }
})

test('a patch without a structured header reports incomplete instead of guessing', () => {
  const review = facts({ toolName: 'apply_patch', arguments: { patch: 'garbage without headers' } })
  assert.equal(review.reviewAction, 'Apply patch')
  assert.equal(review.reviewTarget, '')
  assert.equal(review.reviewImpact, 'Whole file')
  assert.equal(review.reviewAvailability, 'incomplete')
})
