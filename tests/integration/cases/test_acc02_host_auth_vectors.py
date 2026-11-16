"""ACC-02: the Go verifier and the Host JavaScript signer share one contract."""

import json
import os
import subprocess

import pytest

from environment import ROOT

pytestmark = pytest.mark.protocol

VECTORS = ROOT / "access/internal/auth/testdata/host_auth_vectors.json"

NODE_SCRIPT = r"""
import fs from 'node:fs'
import { authorizationPublicKey, hostAuthMessage, signHostAuth } from './host/runtime/src/security/host-auth.js'
import { validatePairingSignatureTime } from './host/runtime/src/contracts/time.js'

const vectors = JSON.parse(fs.readFileSync(process.env.WHALEREIN_AUTH_VECTORS, 'utf8'))
const seed = Buffer.from(vectors.seed_hex, 'hex')
const publicKey = Buffer.from(authorizationPublicKey(seed)).toString('hex')
const results = {
  publicKeyMatches: publicKey === vectors.public_key_hex,
  cases: [],
}
for (const item of vectors.cases) {
  const message = Buffer.from(hostAuthMessage(item.purpose, item.host_id, item.device_id, item.signed_at_unix_ms)).toString('utf8')
  const signature = Buffer.from(signHostAuth(seed, item.purpose, item.host_id, item.device_id, item.signed_at_unix_ms)).toString('hex')
  results.cases.push({
    messageMatches: message === item.message,
    signatureMatches: signature === item.signature_hex,
  })
}
const now = 1700000000000
results.window = {
  pastAccepted: validatePairingSignatureTime(now - vectors.window.past_ms, now).ok === true,
  pastRejected: validatePairingSignatureTime(now - vectors.window.past_ms - 1, now).ok === false,
  futureAccepted: validatePairingSignatureTime(now + vectors.window.future_ms, now).ok === true,
  futureRejected: validatePairingSignatureTime(now + vectors.window.future_ms + 1, now).ok === false,
}
console.log(JSON.stringify(results))
"""


@pytest.mark.description(
    "The Host JavaScript signer produces the shared vector messages and signatures byte for byte, including the two-minute past and one-minute future clock window boundaries."
)
def test_host_signer_matches_the_shared_vectors():
    assert VECTORS.is_file(), VECTORS
    vectors = json.loads(VECTORS.read_text())
    result = subprocess.run(
        ["node", "--input-type=module", "-e", NODE_SCRIPT],
        cwd=ROOT,
        env={**os.environ, "WHALEREIN_AUTH_VECTORS": str(VECTORS)},
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    output = json.loads(result.stdout)
    assert output["publicKeyMatches"], output
    assert len(output["cases"]) == len(vectors["cases"])
    for index, case in enumerate(output["cases"]):
        assert case["messageMatches"], ("message", index, vectors["cases"][index])
        assert case["signatureMatches"], ("signature", index, vectors["cases"][index])
    assert output["window"] == {
        "pastAccepted": True,
        "pastRejected": True,
        "futureAccepted": True,
        "futureRejected": True,
    }, output["window"]
