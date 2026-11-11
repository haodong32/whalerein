// Device display names are labels, not topic ids. They may contain spaces
// and letters outside ASCII. Control characters are dropped.

const MAX_DISPLAY_NAME_LENGTH = 64

export function sanitizeDisplayName(value) {
  const text = String(value ?? '').replace(/[\u0000-\u001F\u007F]/g, '').trim()
  return [...text].slice(0, MAX_DISPLAY_NAME_LENGTH).join('').trim()
}
