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

/**
 * Directory OS family for a Host. Deliberately coarse: only the family is
 * published, never a patch, build or kernel version.
 * @param {NodeJS.Platform|string} [platform]
 */
export function hostOsFamily(platform = process.platform) {
  switch (String(platform)) {
    case 'darwin': return 'macOS'
    case 'win32': return 'Windows'
    case 'linux': return 'Linux'
    default: return ''
  }
}
