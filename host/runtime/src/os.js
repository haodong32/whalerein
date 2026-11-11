const OS_TOKENS = new Set([
  'linux',
  'macos',
  'ios',
  'freebsd',
  'dragonfly',
  'netbsd',
  'openbsd',
  'solaris',
  'android',
  'windows',
])

export function sanitizeOs(value) {
  const text = String(value ?? '').trim().toLowerCase()
  return OS_TOKENS.has(text) ? text : ''
}
