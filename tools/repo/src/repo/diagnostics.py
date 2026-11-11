"""Shared redaction and size limits for diagnostic output, not successful results."""

from __future__ import annotations

import re

LIMIT = 8000
REDACTED = "<redacted>"
_SENSITIVE_KEY = re.compile(
    r"(?i)(?:[\w.-]*(?:token|password|passwd|secret|key|credential)s?"
    r"|authorization|message[_-]?(?:content|body)|tool[_-]?(?:parameters|arguments)"
    r"|invitation[_-]?(?:uri|url))\Z"
)
_FIELD = re.compile(r"""(?P<prefix>(?<![\w])(?:["']?)(?P<key>[\w.-]+)(?:["']?)\s*[:=]\s*)""")
_PEM = re.compile(
    r"-----BEGIN [^-\n]*PRIVATE KEY-----[\s\S]*?"
    r"(?:-----END [^-\n]*PRIVATE KEY-----|\Z)"
)
_AUTH = re.compile(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+")
_INVITATION = re.compile(r"(?i)whalerein://pair[^\s\"'<>]*")
_URL_USERINFO = re.compile(r"(?i)(\b[a-z][a-z0-9+.-]*://)[^/\s@]+@")


def sensitive_key(value: str) -> bool:
    return _SENSITIVE_KEY.fullmatch(value) is not None


def _value_end(text: str, start: int, key: str) -> int:
    """Consume quoted/structured values as a whole, including escaped quotes."""
    if start == len(text):
        return start
    if text[start] in "\"'[{(":
        quote = text[start] if text[start] in "\"'" else None
        depth = 0 if quote else 1
        escaped = False
        for index in range(start + 1, len(text)):
            char = text[index]
            if escaped:
                escaped = False
            elif char == "\\" and quote:
                escaped = True
            elif quote:
                if char == quote:
                    quote = None
                    if depth == 0:
                        return index + 1
            elif char in "\"'":
                quote = char
            elif char in "[{(":
                depth += 1
            elif char in "]})":
                depth -= 1
                if depth == 0:
                    return index + 1
        return len(text)
    if re.fullmatch(r"(?i)(?:message[_-]?(?:content|body)|tool[_-]?(?:parameters|arguments))", key):
        # Unquoted business payloads have no reliable end delimiter: fail closed.
        return len(text)
    end = re.search(r"[\s&,;}\]]", text[start:])
    return start + end.start() if end else len(text)


def redact(value: str) -> str:
    """Mask recognizable credentials and labeled business payloads before rendering."""
    text = _PEM.sub("<redacted private key>", value)
    text = _INVITATION.sub("<redacted invitation>", text)
    text = _URL_USERINFO.sub(lambda match: match[1] + REDACTED + "@", text)
    text = _AUTH.sub(lambda match: match[1] + " " + REDACTED, text)
    fragments = []
    cursor = 0
    for match in _FIELD.finditer(text):
        if match.start() < cursor or not sensitive_key(match["key"]):
            continue
        fragments.extend((text[cursor : match.end()], REDACTED))
        cursor = _value_end(text, match.end(), match["key"])
    fragments.append(text[cursor:])
    return "".join(fragments)


def sanitize(value: object) -> str:
    text = redact(str(value))
    return text[:LIMIT] + ("\n[diagnostic truncated]" if len(text) > LIMIT else "")


def sanitize_data(value):
    """Preserve JSON structure while masking sensitive fields and sanitizing text."""
    if isinstance(value, dict):
        return {
            sanitize(key): REDACTED if sensitive_key(str(key)) else sanitize_data(item)
            for key, item in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [sanitize_data(item) for item in value]
    return sanitize(value) if isinstance(value, str) else value
