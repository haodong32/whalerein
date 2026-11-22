// Full synthetic plugin used by the opt-in identity/transport stack test.
import { createHost } from '../../../../host/dsh/index.js'
import { FakeDshAdapter } from '../../../../host/dsh/src/adapter/fake.js'
import { silentLogger } from '../../../../host/runtime/src/log.js'
const adapter=new FakeDshAdapter({sessions:[{sessionId:'session-flow',running:false}]})
const host=createHost({adapter:'fake',mqtt_url:process.env.WHALEREIN_TEST_MQTT_URL,host_id:'host-flow',host_token:'fixture-host',state_dir:process.env.WHALEREIN_TEST_STATE_DIR,access_url:process.env.WHALEREIN_ACCESS_URL,allow_insecure_mqtt:true,event_batch_ms:1,log_level:'silent'},{adapter,logger:silentLogger})
const alive=setInterval(()=>{},1000)
await host.start();await host.access.waitForConnected(10000)
adapter.emit('session-flow',{type:'assistant/message',kind:'output',data:{text:'x'.repeat(2*1024*1024)}})
process.stdout.write('runtime-ready\n')
process.stdin.setEncoding('utf8')
process.stdin.on('data',data=>{
  if(data.includes('count'))process.stdout.write(`peers:${host.store.countPeerKeys()}\n`)
  if(data.includes('emit'))adapter.emit('session-flow',{type:'assistant/message',kind:'output',data:{text:'x'.repeat(2*1024*1024)}})
})
process.on('SIGTERM',async()=>{clearInterval(alive);await host.stop();process.exit(0)})
