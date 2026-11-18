#!/usr/bin/env node
// AC-14 artifact conformance against the compiled protobuf descriptor and the
// canonical version-one vectors. This checker never defines a second schema:
// it loads the committed compiled descriptor used by the Host runtime, then
// applies the published required-field rules.
//
// Usage: node required_fields.mjs --check

import assert from 'node:assert'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '../../../..')
const require = createRequire(resolve(root, 'host/runtime/package.json'))
const protobuf = require('protobufjs')

const descriptor = JSON.parse(
  readFileSync(resolve(root, 'host/runtime/src/generated/whalerein.json'), 'utf8'),
)
const protoRoot = protobuf.Root.fromJSON(descriptor)
const namespace = protoRoot.lookup('whalerein.v1')
if (!namespace?.nested) throw new Error('compiled descriptor is missing whalerein.v1')

const vectors = JSON.parse(
  readFileSync(resolve(root, 'tests/fixtures/proto/messages.json'), 'utf8'),
).messages
const byCase = new Map(vectors.map(vector => [vector.caseId, vector]))

function camel(name) {
  return name.replace(/_([a-z0-9])/g, (_, letter) => letter.toUpperCase())
}

function messageType(name) {
  const type = protoRoot.lookupType(`whalerein.v1.${name}`)
  assert(type, `compiled descriptor is missing ${name}`)
  return type
}

function convert(type, value) {
  if (value === null || value === undefined) return value
  if (Array.isArray(value)) return value.map(item => convert(type, item))
  if (typeof value !== 'object') return value
  const out = {}
  for (const [key, item] of Object.entries(value)) {
    const field = type.fieldsArray.find(
      candidate => candidate.name === key || camel(candidate.name) === key,
    )
    if (!field) throw new Error(`${type.fullName} has no field for JSON key ${key}`)
    if (field.repeated) out[field.name] = item.map(entry => convertField(field, entry))
    else out[field.name] = convertField(field, item)
  }
  return out
}

function convertField(field, value) {
  if (value === null || value === undefined) return value
  const nested = field.resolvedType
  if (nested && typeof nested.values === 'object' && typeof value === 'string') {
    return nested.values[value] ?? value
  }
  if (['int64', 'uint64', 'sint64', 'fixed64', 'sfixed64'].includes(field.type) && typeof value === 'string') {
    return Number(value)
  }
  if (nested && nested.fields) return convert(nested, value)
  return value
}

function encode(name, json) {
  const type = messageType(name)
  const value = convert(type, json)
  const error = type.verify(value)
  if (error) throw new Error(`${name}: ${error}`)
  return Buffer.from(type.encode(type.fromObject(value)).finish())
}

function decode(name, bytes) {
  return messageType(name).toObject(messageType(name).decode(bytes), {
    longs: String,
    enums: String,
    bytes: String,
    defaults: false,
  })
}

function required(condition, code, detail) {
  if (!condition) {
    const error = new Error(`${code}: ${detail}`)
    error.code = code
    throw error
  }
}

// The published contract from proto/v1/WIRE.md: an absent interests message is
// invalid_argument, not an implicit conversation or empty-questions selection.
function requireInterests(session) {
  required(session.interests !== undefined, 'invalid_argument', 'WatchSession.interests is required')
  required(
    session.interests.conversation === true || session.interests.questions === true,
    'invalid_argument',
    'WatchSession.interests selects no facet',
  )
  return session.interests
}

// A complete current collection requires an available descriptor stamp and a
// page stamp; absence must never become an empty Questions collection.
function requirePage(page) {
  required(page.stamp !== undefined, 'invalid_argument', 'QuestionPage.stamp is required')
  required(page.complete === true, 'invalid_argument', 'QuestionPage.complete is required on the final page')
  required(page.next_cursor === undefined || page.next_cursor === '', 'invalid_argument', 'complete page carries a cursor')
  return page
}

function checkMatchingArtifacts() {
  const watch = byCase.get('typed-watch-interests')
  const page = byCase.get('typed-question-page')
  const descriptorVector = byCase.get('typed-question-descriptor')
  const questionsOnly = byCase.get('typed-watch-questions-only')
  for (const vector of [watch, page, descriptorVector, questionsOnly]) {
    assert(vector, 'canonical version-one vector is missing')
  }

  // Matching required fields are accepted and survive the compiled wire.
  const session = decode('WatchSession', encode('WatchSession', watch.json))
  const interests = requireInterests(session)
  assert.equal(interests.conversation, true)
  assert.equal(interests.questions, true)

  const decodedPage = decode('QuestionPage', encode('QuestionPage', page.json))
  requirePage(decodedPage)
  assert.equal(decodedPage.stamp.observation_epoch, 'observation-epoch-1')
  assert.equal(decodedPage.stamp.revision, '7')
  assert.equal(decodedPage.batches.length, 1)

  const ready = decode('WatchReady', encode('WatchReady', questionsOnly.json))
  assert(ready.questions, 'questions-only WatchReady requires a descriptor')
  assert.equal(ready.state, undefined)
  assert.equal(ready.questions.availability, 'QUESTION_AVAILABILITY_AVAILABLE')
  assert(ready.questions.stamp, 'available WatchReady descriptor requires a stamp')
}

async function checkMissingRequiredFields() {
  const source = await import(
    pathToFileURL(resolve(root, 'host/runtime/src/contracts/source.js')).href
  )

  // Missing WatchSession.interests stays distinguishable from an explicit
  // empty selection and is rejected by the published rule.
  const missingInterests = decode('WatchSession', encode('WatchSession', { sessionId: 's' }))
  assert.equal(missingInterests.interests, undefined, 'compiled decoder fabricated interests')
  assert.throws(() => requireInterests(missingInterests), error => error.code === 'invalid_argument')

  // Available descriptor without a stamp is not an empty collection.
  const descriptorVector = byCase.get('typed-question-descriptor')
  const availableWithoutStamp = { availability: descriptorVector.json.availability }
  assert.throws(
    () => source.validateQuestionDescriptor(availableWithoutStamp),
    error => error.code === 'invalid_argument',
  )

  // UNSPECIFIED is invalid on the wire for this contract.
  assert.throws(
    () => source.validateQuestionDescriptor({ availability: 'QUESTION_AVAILABILITY_UNSPECIFIED' }),
    error => error.code === 'invalid_argument',
  )

  // Page stamp and complete are required; absence never becomes Current-empty.
  const pageVector = byCase.get('typed-question-page')
  const pageWithoutStamp = { ...pageVector.json }
  delete pageWithoutStamp.stamp
  const decodedWithoutStamp = decode('QuestionPage', encode('QuestionPage', pageWithoutStamp))
  assert.equal(decodedWithoutStamp.stamp, undefined, 'compiled decoder fabricated a page stamp')
  assert.throws(() => requirePage(decodedWithoutStamp), error => error.code === 'invalid_argument')

  const pageWithoutComplete = { ...pageVector.json }
  delete pageWithoutComplete.complete
  delete pageWithoutComplete.nextCursor
  const decodedWithoutComplete = decode('QuestionPage', encode('QuestionPage', pageWithoutComplete))
  assert.equal(decodedWithoutComplete.complete, undefined, 'compiled decoder fabricated a complete marker')
  assert.throws(() => requirePage(decodedWithoutComplete), error => error.code === 'invalid_argument')
}

function checkNoFallbackOrAlias() {
  const names = Object.keys(namespace.nested)
  assert.equal(names.filter(name => name === 'WatchSession').length, 1)
  for (const name of names) {
    assert.doesNotMatch(
      name,
      /(legacy|compat|deprecated|V2$|_v2$)/i,
      `compatibility alias message ${name} must not exist`,
    )
  }
  const sessionFields = messageType('WatchSession').fieldsArray.map(field => field.name).sort()
  assert.deepEqual(sessionFields, ['interests', 'session_id'])
  const pageFields = messageType('QuestionPage').fieldsArray.map(field => field.name).sort()
  assert.deepEqual(pageFields, ['batches', 'complete', 'next_cursor', 'stamp'])
  for (const field of messageType('WatchSession').fieldsArray) {
    assert.notEqual(field.name, 'conversation', 'WatchSession must not carry an implicit conversation flag')
    assert.notEqual(field.name, 'questions', 'WatchSession must not carry an implicit questions flag')
  }
  for (const name of names) {
    for (const field of namespace.nested[name].fieldsArray ?? []) {
      assert.doesNotMatch(field.name, /(legacy|compat|alias|_v2$)/i, `${name}.${field.name} is an alias path`)
    }
  }
}

function checkNoHiddenSecondVersion() {
  const names = Object.keys(namespace.nested)
  for (const name of names) {
    const nested = namespace.nested[name]
    for (const field of nested.fieldsArray ?? []) {
      if (field.name !== 'protocol_version') continue
      assert.equal(field.type, 'uint32', `${name}.${field.name} must stay a uint32, not a second version enum`)
    }
    const enumNames = Object.keys(nested.nested ?? {})
    for (const enumName of enumNames) {
      assert.doesNotMatch(enumName, /PROTOCOL_VERSION/i, `hidden protocol version enum ${name}.${enumName}`)
    }
  }
  assert.equal(names.filter(name => name === 'WatchSession').length, 1)
  assert.equal(names.filter(name => name === 'QuestionPage').length, 1)
  for (const vector of vectors) {
    if (vector.json?.protocolVersion === undefined) continue
    assert.equal(vector.json.protocolVersion, 1, `${vector.caseId} carries a non-version-one protocol`)
  }
  // Exactly one watch and one question-page contract exist; a compatibility
  // reader or a second version would need another message name.
  for (const required of ['WatchSession', 'WatchReady', 'QuestionDescriptor', 'QuestionPage', 'QuestionStamp']) {
    assert.equal(names.filter(name => name === required).length, 1, `exactly one ${required} message is required`)
  }
}

async function main() {
  const mode = process.argv[2]
  assert(
    ['--check', '--check-matching', '--check-missing', '--check-invariants'].includes(mode),
    'usage: required_fields.mjs --check|--check-matching|--check-missing|--check-invariants',
  )
  if (mode === '--check' || mode === '--check-matching') checkMatchingArtifacts()
  if (mode === '--check' || mode === '--check-missing') await checkMissingRequiredFields()
  if (mode === '--check' || mode === '--check-invariants') {
    checkNoFallbackOrAlias()
    checkNoHiddenSecondVersion()
  }
  process.stdout.write(`artifact-conformance-ok ${mode.slice(2)}\n`)
}

main().catch(error => {
  process.stderr.write(`artifact conformance failed: ${error.message}\n`)
  process.exit(1)
})
