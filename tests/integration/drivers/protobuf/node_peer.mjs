#!/usr/bin/env node
// WhaleRein protobuf JSON/binary peer using the committed protobufjs descriptor.
//
// Usage:
//   node node_peer.mjs --write <fixture.json>
//   node node_peer.mjs --check <fixture.json>
//
// The fixture stores proto3 JSON objects and the expected binary hex. Rust
// (prost + pbjson) verifies the same fixture in rust-peer.

import { readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert'

const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '../../../..')
const require = createRequire(resolve(root, 'host/runtime/package.json'))
const protobuf = require('protobufjs')

const descriptorPath = resolve(root, 'host/runtime/src/generated/whalerein.json')
const protoPath = resolve(root, 'proto/v1/whalerein.proto')

function loadRoot() {
  return protobuf.Root.fromJSON(JSON.parse(readFileSync(descriptorPath, 'utf8')))
}

function camel(name) {
  return name.replace(/_([a-z0-9])/g, (_, letter) => letter.toUpperCase())
}

function findField(type, key) {
  return type.fieldsArray.find((field) => field.name === key || camel(field.name) === key)
}

function jsonToValue(type, json) {
  const out = {}
  for (const [key, value] of Object.entries(json)) {
    const field = findField(type, key)
    if (!field) throw new Error(`${type.fullName} has no field for JSON key ${key}`)
    out[field.name] = convert(field, value)
  }
  return out
}

function convert(field, value) {
  if (value === null || value === undefined) return value
  if (field.map) return Object.fromEntries(Object.entries(value).map(([key,item])=>[key,convertOne(field,item)]))
  if (field.repeated) return value.map((item) => convertOne(field, item))
  return convertOne(field, value)
}

function convertOne(field, value) {
  const nested = field.resolvedType
  if (nested instanceof protobuf.Enum) {
    return typeof value === 'string' ? nested.values[value] : value
  }
  if (
    ['int64', 'uint64', 'sint64', 'fixed64', 'sfixed64'].includes(field.type) &&
    typeof value === 'string'
  ) {
    return Number(value)
  }
  if (nested && nested.fields) return jsonToValue(nested, value)
  return value
}

function valueToJson(type, message) {
  const object = type.toObject(message, {
    longs: String,
    enums: String,
    bytes: String,
    defaults: false,
  })
  return objectToJson(type, object)
}

function objectToJson(type, object) {
  const out = {}
  for (const field of type.fieldsArray) {
    if (!(field.name in object)) continue
    const value = object[field.name]
    out[camel(field.name)] = field.map
      ? Object.fromEntries(Object.entries(value).map(([key,item])=>[key,oneToJson(field,item)]))
      : field.repeated
      ? value.map((item) => oneToJson(field, item))
      : oneToJson(field, value)
  }
  return out
}

function oneToJson(field, value) {
  const nested = field.resolvedType
  if (nested && nested.fields) return objectToJson(nested, value)
  return value
}

function encode(type, json) {
  const value = jsonToValue(type, json)
  const error = type.verify(value)
  if (error) throw new Error(`${type.fullName}: ${error}`)
  return Buffer.from(type.encode(type.fromObject(value)).finish())
}

function roundtrip(type, binary) {
  return valueToJson(type, type.decode(binary))
}

const messages = [
  {
    caseId: 'host-request',
    description: "When {language} round-trips the host-request vector (HostRequest), routing metadata, digest and encrypted body match the committed binary and JSON.",
    name: 'whalerein.v1.HostRequest',
    json: {
      requestId: 'req-1',
      hostId: 'host-1',
      deviceId: 'device-1',
      kind: 'REQUEST_KIND_COMMAND',
      commandId: 'cmd-1',
      payloadDigest: 'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=',
      expiresAtUnixMs: '1700000000000',
      traceId: 'trace-1',
      protocolVersion: 1,
      channelId: 'channel-1',
      body: 'bm9pc2UtYnl0ZXM=',
    },
  },
  {
    caseId: 'host-result-success',
    description: "When {language} round-trips the host-result-success vector (HostResult), command status and encrypted result bytes match the committed binary and JSON.",
    name: 'whalerein.v1.HostResult',
    json: {
      requestId: 'req-1',
      hostId: 'host-1',
      deviceId: 'device-1',
      status: 'host_accepted',
      commandId: 'cmd-1',
      payloadDigest: 'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=',
      resultBody: 'cmVzdWx0LWJ5dGVz',
      traceId: 'trace-1',
      completedAtUnixMs: '1700000000123',
      protocolVersion: 1,
      channelId: 'channel-1',
    },
  },
  {
    caseId: 'host-result-error',
    description: "When {language} round-trips the host-result-error vector (HostResult), failure status and error details match the committed binary and JSON.",
    name: 'whalerein.v1.HostResult',
    json: {
      requestId: 'req-2',
      hostId: 'host-1',
      deviceId: 'device-1',
      status: 'failed',
      errorCode: 'payload_digest_mismatch',
      errorMessage: 'digest conflict',
      traceId: 'trace-2',
      completedAtUnixMs: '1700000000456',
      protocolVersion: 1,
    },
  },
  {
    caseId: 'request-state',
    description: "When {language} round-trips the request-state vector (RequestState), persisted request metadata and status match the committed binary and JSON.",
    name: 'whalerein.v1.RequestState',
    json: {
      requestId: 'req-1',
      hostId: 'host-1',
      deviceId: 'device-1',
      status: 'succeeded',
      commandId: 'cmd-1',
      payloadDigest: 'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=',
      resultBody: 'cmVzdWx0LWJ5dGVz',
      expiresAtUnixMs: '1700000000000',
      updatedAtUnixMs: '1700000000500',
      channelId: 'channel-1',
    },
  },
  {
    caseId: 'pairing-claim-request',
    description: "When {language} round-trips the pairing-claim-request vector (PairingClaimRequest), pairing identifiers and handshake data match the committed binary and JSON.",
    name: 'whalerein.v1.PairingClaimRequest',
    json: {
      token: 'pairing-token-1',
      claimId: 'claim-1',
      phase: 'PAIRING_PHASE_HELLO',
      noiseMessage: 'bXNnMQ==',
      deviceId: 'device-1',
      protocolVersion: 1,
      confirmed: true,
    },
  },
  {
    caseId: 'pairing-claim-response',
    description: "When {language} round-trips the pairing-claim-response vector (PairingClaimResponse), host identity and handshake response match the committed binary and JSON.",
    name: 'whalerein.v1.PairingClaimResponse',
    json: {
      claimId: 'claim-1',
      hostId: 'host-1',
      phase: 'PAIRING_PHASE_HELLO',
      noiseMessage: 'bXNnMg==',
      hostPublicKey: 'aG9zdC1wdWJsaWMta2V5',
      hostFingerprint: 'fp-1',
      deviceId: 'device-1',
      devicePublicKey: 'ZGV2aWNlLXB1YmxpYy1rZXk=',
      deviceToken: 'device-token-1',
      expiresAtUnixMs: '1700000000000',
      protocolVersion: 1,
    },
  },
  {
    caseId: 'encrypted-frame',
    description: "When {language} round-trips the encrypted-frame vector (EncryptedFrame), nonce and ciphertext match the committed binary and JSON.",
    name: 'whalerein.v1.EncryptedFrame',
    json: {
      channelId: 'channel-1',
      nonce: 'AAECAwQFBgcICQoL',
      ciphertext: 'Y2lwaGVydGV4dA==',
    },
  },
  {
    caseId: 'host-list',
    description: "When {language} round-trips the host-list vector (HostList), host identities and online states match the committed binary and JSON.",
    name: 'whalerein.v1.HostList',
    json: {
      hosts: [
        { hostId: 'host-1', online: true, protocolVersion: 1, fingerprint: 'fp-1' },
        { hostId: 'host-2', online: true, protocolVersion: 1, fingerprint: 'fp-2' },
      ],
    },
  },
  {
    caseId: 'capability-set',
    description: "When {language} round-trips the capability-set vector (CapabilitySet), advertised capabilities and protocol version match the committed binary and JSON.",
    name: 'whalerein.v1.CapabilitySet',
    json: {
      capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync'],
      protocolVersion: 1,
    },
  },
  {
    caseId: 'revocation-notice',
    description: "When {language} round-trips the revocation-notice vector (RevocationNotice), device identity and revocation metadata match the committed binary and JSON.",
    name: 'whalerein.v1.RevocationNotice',
    json: {
      replayId: 'replay-1',
      deviceIds: ['device-1', 'device-2'],
      final: true,
      revokedAtUnixMs: '1700000000000',
      grantGenerations: {'device-1':'1','device-2':'2'},
    },
  },
  {
    caseId: 'approval',
    description: "When {language} round-trips the approval vector (Approval), approval identity, digest, version and pending state match the committed binary and JSON.",
    name: 'whalerein.v1.Approval',
    json: {
      approvalId: 'approval-1',
      sessionId: 'session-1',
      toolName: 'shell',
      callId: 'call-1',
      reason: 'needs approval',
      version: '1',
      digest: 'ZGlnZXN0',
      expiresAtUnixMs: '1700000000000',
      state: 'APPROVAL_STATE_PENDING',
    },
  },
  {caseId:'typed-live', description:'When {language} round-trips a typed live update, scoped attempt identity, inclusive sequence interval and Unicode text survive without native JSON.', name:'whalerein.v1.EventPayload',json:{update:{watchId:'watch-1',session:{hostId:'h',sessionId:'s'},live:{attempt:{liveEpoch:'epoch',attemptId:'a'},blockId:'text',firstSeq:'1',lastSeq:'2',delta:{text:'你🙂'}}}}},
  {caseId:'typed-history',description:'When {language} round-trips a typed history page, opaque cursor boundaries, message blocks and attempt origin remain identical.',name:'whalerein.v1.HistoryPage',json:{session:{hostId:'h',sessionId:'s'},batch:{fromCursor:'opaque-alpha',toCursor:'opaque-beta',batchId:'batch',changes:[{message:{messageId:'m',role:'ASSISTANT',blocks:[{blockId:'text',text:{text:'x'}}]},origin:{liveEpoch:'epoch',attemptId:'a'}}]},cutCursor:'opaque-beta',complete:true,snapshot:true}},
  {caseId:'typed-state',description:'When {language} round-trips typed session state, the source epoch, revision, history watermark and run phase remain distinct.',name:'whalerein.v1.SessionState',json:{session:{hostId:'h',sessionId:'s'},liveEpoch:'epoch',revision:'42',historyHead:'opaque-beta',runPhase:'RUNNING'}},
  {caseId:'typed-history-read',description:'When {language} round-trips a typed history request, snapshot selection and independent byte and message budgets are preserved.',name:'whalerein.v1.RequestPayload',json:{readHistory:{sessionId:'s',snapshot:true,maxBytes:262144,maxMessages:100}}},
  {caseId:'typed-watch-ready',description:'When {language} round-trips a watch baseline, the lease and authoritative source watermark remain correlated with the watch identity.',name:'whalerein.v1.ResponsePayload',json:{watchReady:{watchId:'watch-1',state:{session:{hostId:'h',sessionId:'s'},liveEpoch:'epoch',revision:'1',historyHead:'opaque-beta',runPhase:'IDLE'},leaseMs:30000}}},
  {caseId:'typed-source-capabilities',description:'When {language} round-trips source capabilities, history, live text, approvals and explicit delivery modes remain independently declared.',name:'whalerein.v1.SourceCapabilities',json:{history:true,resumableHistory:true,liveText:true,interrupt:true,approvals:true,deliveryModes:['START_IF_IDLE']}},
  {caseId:'typed-question-descriptor',description:'When {language} round-trips an available question descriptor, its observation epoch and monotonic revision survive as one stamp.',name:'whalerein.v1.QuestionDescriptor',json:{availability:'QUESTION_AVAILABILITY_AVAILABLE',stamp:{observationEpoch:'observation-epoch-1',revision:'7'}}},
  {caseId:'typed-question-descriptor-unavailable',description:'When {language} round-trips an unavailable question descriptor, it stays distinct from an empty current collection.',name:'whalerein.v1.QuestionDescriptor',json:{availability:'QUESTION_AVAILABILITY_UNAVAILABLE',unavailableCode:'unavailable'}},
  {caseId:'typed-question-page',description:'When {language} round-trips a stamped question page, the page stamp, final-page marker and source order remain correlated.',name:'whalerein.v1.QuestionPage',json:{batches:[{sessionId:'s',batchId:'b1',sourceEpoch:'epoch-1',version:'3',digest:'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=',phase:'QUESTION_CONTINUED',questions:[{id:'q1',question:'proceed?',options:[{label:'yes'}],intent:{kind:'approval',approve:'yes'}}],answerable:true}],stamp:{observationEpoch:'observation-epoch-1',revision:'7'},complete:true}},
  {caseId:'typed-question-page-unavailable-detail',description:'When {language} round-trips an oversized indivisible batch, the page keeps bounded unavailable metadata instead of truncating content.',name:'whalerein.v1.QuestionPage',json:{batches:[{sessionId:'s',batchId:'b1',sourceEpoch:'epoch-1',version:'3',digest:'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=',phase:'QUESTION_OPEN',contentUnavailable:true}],stamp:{observationEpoch:'observation-epoch-1',revision:'8'},complete:true}},
  {caseId:'typed-watch-interests',description:'When {language} round-trips watch interests, conversation and questions remain explicit independent selections.',name:'whalerein.v1.WatchSession',json:{sessionId:'s',interests:{conversation:true,questions:true}}},
  {caseId:'typed-watch-questions-only',description:'When {language} round-trips a questions-only watch baseline, it carries a descriptor without a conversation state.',name:'whalerein.v1.WatchReady',json:{watchId:'watch-1',leaseMs:30000,questions:{availability:'QUESTION_AVAILABILITY_AVAILABLE',stamp:{observationEpoch:'observation-epoch-1',revision:'1'}}}},
  {caseId:'typed-session-state-questions',description:'When {language} round-trips conversation state with a question descriptor, the two facets keep independent stamps.',name:'whalerein.v1.SessionState',json:{session:{hostId:'h',sessionId:'s'},liveEpoch:'epoch',revision:'42',historyHead:'opaque-beta',runPhase:'RUNNING',questions:{availability:'QUESTION_AVAILABILITY_AVAILABLE',stamp:{observationEpoch:'observation-epoch-1',revision:'9'}}}},

]

function buildFixture() {
  const rootType = loadRoot()
  rootType.resolveAll()
  return {
    comment: 'Generated from proto/v1/whalerein.proto. Test vectors only.',
    schema: 'whalerein/proto-json/v1',
    messages: messages.map(({ caseId, description, name, json }) => {
      const type = rootType.lookupType(name)
      const binary = encode(type, json)
      const decoded = roundtrip(type, binary)
      assert.deepStrictEqual(decoded, json, `${name} JSON roundtrip differs`)
      return { caseId, description, name, json, hex: binary.toString('hex') }
    }),
  }
}

function checkFixture(path) {
  const fixture = JSON.parse(readFileSync(path, 'utf8'))
  const rootType = loadRoot()
  rootType.resolveAll()
  let checked = 0
  for (const entry of fixture.messages) {
    const type = rootType.lookupType(entry.name)
    const binary = encode(type, entry.json)
    assert.strictEqual(binary.toString('hex'), entry.hex, `${entry.name} binary differs`)
    assert.deepStrictEqual(roundtrip(type, binary), entry.json, `${entry.name} JSON differs`)
    checked += 1
  }
  console.log(`Node.js protobuf encoding and roundtrips match fixtures (${checked} messages)`)
  return checked
}

const argv = process.argv.slice(2)
if (argv[0] === '--write') {
  const fixture = buildFixture()
  writeFileSync(argv[1], JSON.stringify(fixture, null, 2) + '\n')
  console.log(`wrote ${argv[1]} (${fixture.messages.length} messages)`)
} else if (argv[0] === '--check') {
  checkFixture(argv[1])
} else if (argv[0] === '--self-descriptor-check') {
  function signatures(root) {
    root.resolveAll()
    const result = {}
    for (const type of root.nestedArray.flatMap(function walk(namespace) {
      const found = []
      if (namespace instanceof protobuf.Type) found.push(namespace)
      if (namespace.nestedArray) found.push(...namespace.nestedArray.flatMap(walk))
      return found
    })) {
      result[type.fullName] = Object.fromEntries(
        type.fieldsArray.map((field) => [
          field.name.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`),
          {
            id: field.id,
            type: field.type,
            rule: field.repeated ? 'repeated' : field.map ? 'map' : 'optional',
          },
        ]),
      )
    }
    return result
  }
  const fromJson = signatures(loadRoot())
  const parsed = signatures(protobuf.parse(readFileSync(protoPath, 'utf8')).root)
  assert.deepStrictEqual(fromJson, parsed)
  console.log('committed JavaScript descriptor fields match proto/v1/whalerein.proto')
} else {
  console.error('usage: node_peer.mjs --write|--check|--self-descriptor-check <fixture>')
  process.exit(2)
}
