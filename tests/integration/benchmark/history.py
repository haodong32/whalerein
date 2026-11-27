"""History continuation traversal and attachment verification.

One logical history read follows every continuation under a *single*
deadline. Verified logical bytes are counted once, on a fully verified
sequence; retries and physical bytes are tracked separately. Duplicate or
repeated continuation tokens, corrupt content and incomplete traversals fail
the read instead of being silently accepted.

The traversal is deliberately callback based so it can be unit-tested with
synthetic pages and no CLI, container or network.

Pure module.
"""

from __future__ import annotations

import json

MAX_PAGES = 64


class PageTimeout(Exception):
    """The single read deadline expired before the traversal completed."""


def verify_sequence(observed, expected):
    """Check order, identity and content against the full logical sequence.

    Returns bounded facts only: counts, first mismatch index and booleans.
    """
    observed = list(observed)
    expected = list(expected)
    mismatch = next(
        (
            index
            for index, (left, right) in enumerate(zip(observed, expected))
            if left != right
        ),
        None,
    )
    if mismatch is None and len(observed) != len(expected):
        mismatch = min(len(observed), len(expected))
    return {
        "verified": mismatch is None,
        "mismatch_index": mismatch,
        "observed_messages": len(observed),
        "expected_messages": len(expected),
    }


def walk(
    fetch, *, expected, page_size, deadline_ms, clock_ms, text_of, max_pages=MAX_PAGES
):
    """Traverse every history continuation under one deadline.

    ``fetch(page_size, continuation, timeout_ms)`` returns one page mapping::

        {"messages": [...], "continuation": token|None, "complete": bool,
         "physical_bytes": int}

    and raises :class:`PageTimeout` when the remaining deadline expires. Text
    extraction is injected so this module stays independent of the CLI
    envelope.

    Returns raw read facts, not a latency or a verdict about the product.
    """
    if page_size < 1:
        raise ValueError("page_size must be positive")
    started = clock_ms()
    pages = []
    physical_bytes = 0
    token_repeats = 0
    corrupt = False
    incomplete = False
    error_code = None
    seen_tokens = set()
    continuation = None
    outcome = "completed"
    pages_count = 0
    try:
        while True:
            remaining = deadline_ms - (clock_ms() - started)
            if remaining <= 0:
                raise PageTimeout
            page = fetch(
                page_size, continuation, max(1, min(int(remaining), deadline_ms))
            )
            pages_count += 1
            physical_bytes += int(page.get("physical_bytes") or 0)
            if page.get("error_code"):
                outcome = "product_error"
                error_code = page["error_code"]
                break
            messages = list(page.get("messages") or [])
            pages.append([text_of(message) for message in messages])
            token = page.get("continuation")
            complete = page.get("complete") is True or not token
            if token is not None:
                if token in seen_tokens:
                    token_repeats += 1
                    corrupt = True
                    break
                seen_tokens.add(token)
            if pages_count > max_pages:
                corrupt = True
                incomplete = True
                break
            if complete:
                break
            if token is None:
                incomplete = True
                break
            continuation = token
    except PageTimeout:
        outcome = "timeout"
        error_code = "timeout"
    except (
        AssertionError,
        json.JSONDecodeError,
        OSError,
        TypeError,
        AttributeError,
        KeyError,
        ValueError,
    ) as error:
        outcome = "product_error"
        code = getattr(error, "code", None)
        error_code = code if isinstance(code, str) and code else None

    # The CLI returns newest page first; each page is chronological. Reverse
    # the page order to reconstruct the logical, oldest-to-newest sequence.
    flattened = [text for page in reversed(pages) for text in page]
    verification = verify_sequence(flattened, expected)
    if outcome == "completed" and (
        not verification["verified"] or incomplete or corrupt
    ):
        outcome = "incorrect_content"
    verified = outcome == "completed" and verification["verified"] and not corrupt
    bytes_expected = sum(len(text.encode()) for text in expected)
    return {
        "pages": pages_count,
        "messages": len(flattened),
        "bytes_expected": bytes_expected,
        "bytes_verified": bytes_expected if verified else 0,
        "physical_bytes": physical_bytes,
        "route": "inline",
        "outcome": outcome,
        "verified": verified,
        "token_repeats": token_repeats,
        "corrupt": corrupt or incomplete or not verification["verified"],
        "error_code": error_code,
        "mismatch_index": verification["mismatch_index"],
    }


def attachment_facts(*, saved_bytes, expected_bytes, expected_sha256, actual_sha256):
    """Verify length and digest while recording only bounded facts.

    Returns length and digest *match* booleans; the digest value itself stays
    out of the case facts.
    """
    length_match = saved_bytes == expected_bytes
    digest_match = (
        actual_sha256 == expected_sha256
        and isinstance(actual_sha256, str)
        and len(actual_sha256) == 64
    )
    verified = length_match and digest_match
    return {
        "bytes_expected": expected_bytes,
        "bytes_verified": saved_bytes if verified else 0,
        "physical_bytes": saved_bytes,
        "route": "reference",
        "outcome": "completed" if verified else "incorrect_content",
        "verified": verified,
        "token_repeats": 0,
        "corrupt": not verified,
        "length_match": length_match,
        "digest_match": digest_match,
    }
