// Exercise the signed host route through the same credential adapter boundary.
// Private key bytes remain in memory and are never emitted.
import { FixtureAdapter } from '../../fixtures/adapter.mjs'
import { HOST_KEY_CREDENTIAL } from '../../../../host/runtime/src/security/identity.js'
import { revokeDevice } from '../../../../host/dsh/src/pairing.js'

const adapter=new FixtureAdapter()
try {
  await adapter.open()
  const credential=await adapter.call('getCredential',HOST_KEY_CREDENTIAL)
  if(typeof credential!=='string')throw new Error('fixture host credential unavailable')
  await revokeDevice({accessUrl:'http://127.0.0.1:18080',hostId:process.env.WHALEREIN_HOST_ID,
    deviceId:process.env.WHALEREIN_DEVICE_ID,privateKey:Buffer.from(credential,'base64')})
  console.log('host-revoke-ok')
} catch {
  console.error('host revocation failed')
  process.exitCode=1
} finally {await adapter.close()}
