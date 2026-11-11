// Container-owned logical-transfer regression against the isolated 1 MiB broker.
import test from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
const mqtt = require('mqtt')
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { LogicalTransport, PACKET_BYTES } from '../../../../host/runtime/src/access/transport.js'
import { codec } from '../../../../host/runtime/src/protocol.js'


test('when a two MiB result is published, the real broker delivers it in bounded frames without reconnecting', {timeout:15000}, async()=>{
  const hostId=process.env.WHALEREIN_TEST_HOST_ID??`packet-budget-${randomUUID()}`
  const url=process.env.WHALEREIN_TEST_MQTT_URL??'mqtt://127.0.0.1:1884'
  const proto=codec();const resultTopic=`whalerein/v1/h/${hostId}/result`;const requestTopic=`whalerein/v1/h/${hostId}/request`
  const subscriber=await mqtt.connectAsync(url,{protocolVersion:5,clientId:`packet-budget-reader-${randomUUID()}`,username:process.env.WHALEREIN_TEST_ACCESS_USERNAME??'access',password:process.env.WHALEREIN_TEST_ACCESS_PASSWORD,reconnectPeriod:0,connectTimeout:3000})
  const packetSizes=[]
  const access=new MqttAccess({url,hostId,token:process.env.WHALEREIN_TEST_HOST_PASSWORD??'host-pass',connect:(url,options)=>{
    const client=mqtt.connect(url,options);const publish=client.publish.bind(client)
    client.publish=(topic,bytes,...args)=>{packetSizes.push(bytes.length);return publish(topic,bytes,...args)}
    return client
  }})
  try {
    await subscriber.subscribeAsync('whalerein/v1/h/+/result',{qos:1})
    const requestId=randomUUID();let resolveResult;let resolveProbe
    const received=new Promise(resolve=>{resolveResult=resolve});const probed=new Promise(resolve=>{resolveProbe=resolve})
    const deliver=(bytes)=>{const result=proto.decode('whalerein.v1.HostResult',bytes);if(result.request_id===requestId)resolveResult(result.result_body.length)}
    const receiver=new LogicalTransport({codec:proto,publish:(topic,bytes)=>subscriber.publishAsync(topic,bytes,{qos:1})})
    subscriber.on('message',(topic,bytes)=>{if(topic!==resultTopic)return;receiver.receive(topic,bytes,requestTopic,deliver).then(consumed=>{if(!consumed)deliver(bytes)}).catch(()=>{})})
    access.start({onRequest:request=>{if(request.request_id==='probe')resolveProbe()}})
    await access.waitForConnected(3000)
    await subscriber.publishAsync(requestTopic,proto.encode('whalerein.v1.HostRequest',{request_id:'probe',host_id:hostId,relay_transport_version:2}),{qos:1})
    await probed
    const body=new Uint8Array(2*1024*1024)
    await access.publishResult(proto.encode('whalerein.v1.HostResult',{request_id:requestId,host_id:hostId,status:'succeeded',result_body:body}))
    const size=await Promise.race([received,new Promise((_,reject)=>setTimeout(()=>reject(new Error('logical result was not delivered')),3000).unref())])
    assert.equal(size,body.length);assert.equal(access.connected,true);assert.equal(access.describe().reconnects,0)
    assert.ok(packetSizes.length>=35);assert.ok(packetSizes.every(size=>size<=PACKET_BYTES))
  } finally {await access.stop();await subscriber.endAsync(true)}
})
