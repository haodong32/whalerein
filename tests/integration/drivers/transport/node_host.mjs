// Synthetic host for the opt-in Go/JS MQTT transport interoperability test.
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { codec } from '../../../../host/runtime/src/protocol.js'
const proto=codec()
const access=new MqttAccess({url:process.env.WHALEREIN_TEST_MQTT_URL,hostId:'h-e2e',token:'host-pass'})
const keepAlive=setInterval(()=>{},1000)
access.start({onRequest:async(request)=>{
  if(request.request_id!=='transport-cross-language')return
  if(request.body.length!==2*1024*1024)throw new Error('incomplete logical request')
  await access.publishResult(proto.encode('whalerein.v1.HostResult',{request_id:request.request_id,host_id:'h-e2e',status:'succeeded',result_body:new Uint8Array(2*1024*1024),protocol_version:1}))
}})
await access.waitForConnected(30_000)
clearInterval(keepAlive)
process.stdout.write('transport-host-ready\n')
process.on('SIGTERM',async()=>{await access.stop();process.exit(0)})
