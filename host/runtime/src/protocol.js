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

const INTERNAL_OPTIONS = Object.freeze({
  longs: Number,
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
      message = type.fromObject(value ?? {})
    } catch {
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
    return type.toObject(message, options)
  }

  /** Decode to proto3-JSON-shaped values: int64 as strings, bytes as base64. */
  decodeJson(typeName, bytes) {
    return this.decode(typeName, bytes, JSON_OPTIONS)
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
