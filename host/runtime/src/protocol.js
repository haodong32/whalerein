// Protobuf codec over the single wire descriptor at
// src/generated/whalerein.json. The plugin loads the descriptor itself rather
// than importing generated DSH packages.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import protobuf from 'protobufjs'
import { PluginError } from './errors.js'

export const DEFAULT_PROTO_PATH = fileURLToPath(
  new URL('./generated/whalerein.json', import.meta.url),
)

// 64-bit values are read as decimal strings and only narrowed to a Number when
// the value is exactly representable. A value beyond 2^53-1 stays a string so a
// version or generation can never be silently corrupted.
const INTERNAL_OPTIONS = Object.freeze({
  longs: String,
  enums: String,
  bytes: Uint8Array,
  defaults: false,
  arrays: true,
  objects: true,
})

const JSON_OPTIONS = Object.freeze({
  longs: String,
  enums: String,
  bytes: String,
  defaults: false,
  arrays: true,
  objects: true,
})

const roots = new Map()

function loadRoot(protoPath) {
  const cached = roots.get(protoPath)
  if (cached) return cached
  let descriptor
  try {
    descriptor = JSON.parse(readFileSync(protoPath, 'utf8'))
  } catch (error) {
    throw new PluginError('internal', `protobuf descriptor unreadable: ${error.code ?? 'read_failed'}`)
  }
  let root
  try {
    root = protobuf.Root.fromJSON(descriptor)
  } catch {
    throw new PluginError('internal', 'protobuf descriptor invalid')
  }
  roots.set(protoPath, root)
  return root
}

export class ProtoCodec {
  #root
  #types = new Map()

  constructor(protoPath = DEFAULT_PROTO_PATH) {
    this.protoPath = protoPath
    this.#root = loadRoot(protoPath)
  }

  type(typeName) {
    const cached = this.#types.get(typeName)
    if (cached) return cached
    let type
    try {
      type = this.#root.lookupType(typeName)
    } catch {
      throw new PluginError('internal', `unknown protobuf type ${typeName}`)
    }
    this.#types.set(typeName, type)
    return type
  }

  /** Encode a plain object into binary protobuf bytes. */
  encode(typeName, value) {
    const type = this.type(typeName)
    // `verify` rejects enum *names* even though fromObject accepts them, so
    // conversion is the validation step here.
    let message
    try {
      this.assertKnownEnums(typeName, value ?? {})
      message = type.fromObject(value ?? {})
    } catch (error) {
      if (error instanceof PluginError) throw error
      throw new PluginError('invalid_argument', 'protobuf value invalid')
    }
    try {
      return type.encode(message).finish()
    } catch {
      throw new PluginError('invalid_argument', 'protobuf value invalid')
    }
  }

  /**
   * Decode binary protobuf into a plain object with enum names, numeric int64
   * values, Uint8Array bytes, and absent defaults omitted.
   */
  decode(typeName, bytes, options = INTERNAL_OPTIONS) {
    const type = this.type(typeName)
    let message
    try {
      message = type.decode(bytes)
    } catch {
      throw new PluginError('invalid_argument', 'protobuf bytes invalid')
    }
    const object = type.toObject(message, options)
    return options === INTERNAL_OPTIONS ? normalizeLongs(type, object) : object
  }

  /** Decode to proto3-JSON-shaped values: int64 as strings, bytes as base64. */
  decodeJson(typeName, bytes) {
    return this.decode(typeName, bytes, JSON_OPTIONS)
  }

  /**
   * Every enum field whose value this build does not know.
   *
   * A decoder may keep an unknown enum as its numeric value, but it must never
   * render it as the zero value; callers turn this list into an explicit
   * protocol_version_incompatible error.
   */
  unknownEnums(typeName, value) {
    const problems = []
    const visit = (type, object, path) => {
      if (!object || typeof object !== 'object') return
      for (const field of type.fieldsArray) {
        const found = fieldValue(field, object)
        if (!found.present) continue
        const nested = field.resolvedType
        const values = field.map
          ? Object.values(found.value)
          : field.repeated
            ? found.value
            : [found.value]
        for (const item of values) {
          if (item === undefined || item === null) continue
          if (nested instanceof protobuf.Enum) {
            const known =
              typeof item === 'string'
                ? nested.values[item] !== undefined
                : nested.valuesById[item] !== undefined
            if (!known) problems.push({ field: `${path}${field.name}`, enum: nested.fullName, value: item })
          } else if (nested && nested.fieldsArray && item && typeof item === 'object') {
            visit(nested, item, `${path}${field.name}.`)
          }
        }
      }
    }
    visit(this.type(typeName), value, '')
    return problems
  }

  /** Reject an unknown enum value with an actionable version error. */
  assertKnownEnums(typeName, value) {
    const [problem] = this.unknownEnums(typeName, value)
    if (problem) {
      throw new PluginError(
        'protocol_version_incompatible',
        `unknown ${problem.enum} value ${String(problem.value)} in ${problem.field}; upgrade the reader`,
      )
    }
    return value
  }

  /** Whether a oneof carries one of its known variants. */
  hasOneof(typeName, value, oneofName) {
    const oneof = this.type(typeName).oneofsArray.find((item) => item.name === oneofName)
    if (!oneof) throw new PluginError('internal', `unknown oneof ${oneofName}`)
    return oneof.fieldsArray.some((field) => value?.[field.name] !== undefined && value?.[field.name] !== null)
  }

  /**
   * Require a known oneof variant. An unknown newer variant decodes as an
   * absent field, so treating it as an empty success would fabricate a result.
   */
  requireOneof(typeName, value, oneofName) {
    if (!this.hasOneof(typeName, value, oneofName)) {
      throw new PluginError(
        'invalid_argument',
        `${typeName} carries no known ${oneofName} variant; check the peer protocol version`,
      )
    }
    return value
  }

  /** Encode proto3-JSON-shaped values (int64 strings, base64 bytes). */
  encodeJsonValue(typeName, jsonValue) {
    const type = this.type(typeName)
    return type.encode(type.fromObject(jsonValue)).finish()
  }

  #enum(typeName) {
    try {
      return this.#root.lookupEnum(typeName)
    } catch {
      throw new PluginError('internal', `unknown protobuf enum ${typeName}`)
    }
  }

  /** One type's enum value name for a number, or undefined. */
  enumName(typeName, value) {
    return this.#enum(typeName).valuesById[value]
  }

  /** One type's numeric enum value for a name, or undefined. */
  enumValue(typeName, name) {
    return this.#enum(typeName).values[name]
  }
}

/** Application protocol window this build supports. */
export const PROTOCOL_WINDOW = Object.freeze({ min: 1, max: 1 })

const LONG_TYPES = new Set(['int64', 'uint64', 'sint64', 'fixed64', 'sfixed64'])

/**
 * Classify a peer application protocol version.
 *
 * Returns `{compatible, side, message}` where `side` is `none`, `peer` or
 * `local`; a mismatch is never retryable and names the side that must upgrade.
 */
export function protocolVersionCompatibility(peer, window = PROTOCOL_WINDOW) {
  const range = `${window.min}..=${window.max}`
  if (peer === 0 || peer === '0' || peer === undefined || peer === null) {
    return {
      compatible: false,
      side: 'peer',
      message: `peer omitted protocol_version (0); upgrade the older peer to protocol version ${window.min} (supported window ${range})`,
    }
  }
  const value = Number(peer)
  if (!Number.isInteger(value)) {
    return {
      compatible: false,
      side: 'peer',
      message: `peer protocol_version ${String(peer)} is not an integer; upgrade the remote peer (supported window ${range})`,
    }
  }
  if (value < window.min) {
    return {
      compatible: false,
      side: 'peer',
      message: `peer protocol version ${value} is older than the supported window ${range}; upgrade the remote Host, Access, or client`,
    }
  }
  if (value > window.max) {
    return {
      compatible: false,
      side: 'local',
      message: `peer protocol version ${value} is newer than this build supports (${range}); upgrade this client`,
    }
  }
  return { compatible: true, side: 'none', message: `protocol version ${value} is supported (${range})` }
}

/** Convert a decoded 64-bit value to BigInt without losing precision. */
export function toBigInt(value) {
  if (typeof value === 'bigint') return value
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) {
      throw new PluginError('invalid_argument', '64-bit value is not exactly representable')
    }
    return BigInt(value)
  }
  try {
    return BigInt(value)
  } catch {
    throw new PluginError('invalid_argument', '64-bit value is not an integer')
  }
}

function narrowLong(field, value) {
  if (LONG_TYPES.has(field.type) && typeof value === 'string') {
    const number = Number(value)
    return Number.isSafeInteger(number) && String(number) === value ? number : value
  }
  return value
}

function camelName(name) {
  return name.replace(/_([a-z0-9])/g, (_, letter) => letter.toUpperCase())
}

function fieldValue(field, object) {
  if (field.name in object) return { present: true, value: object[field.name] }
  const camel = camelName(field.name)
  if (camel in object) return { present: true, value: object[camel] }
  return { present: false }
}

function normalizeLongs(type, object) {
  for (const field of type.fieldsArray) {
    if (!(field.name in object)) continue
    const value = object[field.name]
    if (field.map) {
      for (const key of Object.keys(value)) {
        object[field.name][key] = normalizeFieldValue(field, value[key])
      }
    } else if (field.repeated) {
      object[field.name] = value.map((item) => normalizeFieldValue(field, item))
    } else {
      object[field.name] = normalizeFieldValue(field, value)
    }
  }
  return object
}

function normalizeFieldValue(field, value) {
  const nested = field.resolvedType
  if (nested && nested.fieldsArray && value && typeof value === 'object') {
    return normalizeLongs(nested, value)
  }
  return narrowLong(field, value)
}

let defaultCodec = null

/** Accept either a ProtoCodec instance or a factory and return an instance. */
export function resolveCodec(value) {
  if (!value) return codec()
  if (typeof value === 'function' && typeof value.encode !== 'function') return value()
  return value
}

/** Shared codec for the repository descriptor. */
export function codec(protoPath = DEFAULT_PROTO_PATH) {
  if (protoPath === DEFAULT_PROTO_PATH) {
    defaultCodec ??= new ProtoCodec(protoPath)
    return defaultCodec
  }
  return new ProtoCodec(protoPath)
}
