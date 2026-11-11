// Shared wire addressing; connector configuration belongs to its entry.
export const PROTOCOL_VERSION = 1

export class ConfigError extends Error {
  constructor(code, detail = '') {
    super(detail ? `${code}: ${detail}` : code)
    this.name = 'ConfigError'
    this.code = code
  }
}

export function validateTopicId(value, name) {
  if (typeof value !== 'string' || value.trim() === '') throw new ConfigError('invalid_config', `${name} is required`)
  const text = value
  if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(text)) {
    throw new ConfigError('invalid_config', `${name} contains characters unsafe for an MQTT topic`)
  }
  return text
}

export function topics(hostId) {
  return {
    request: `whalerein/v1/h/${hostId}/request`,
    result: `whalerein/v1/h/${hostId}/result`,
    event: `whalerein/v1/h/${hostId}/event`,
    deviceEvent: (deviceId) => `whalerein/v1/h/${hostId}/d/${deviceId}/event`,
  }
}
