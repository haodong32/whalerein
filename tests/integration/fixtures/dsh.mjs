import { MemoryHostCredentials } from '../../../host/runtime/src/security/credentials.js'
// Isolated simulated DSH authority; no production DSH access.
import net from 'node:net'
import { createInterface } from 'node:readline'
import { FakeDshAdapter } from '../../../host/dsh/src/adapter/fake.js'
import { encode,decode } from './adapter.mjs'
const adapter=new FakeDshAdapter()
const credentials=new MemoryHostCredentials()
const sockets=new Set()
for(const [method,event] of [['subscribeEvents','events'],['subscribeApprovals','approvals']]) {
  adapter[method](value=>{for(const socket of sockets)socket.write(encode({event,value})+'\n')})
}
const methods=new Set(['getCredential','setCredential','nativeHead','listSessions','getSessionSnapshot','listApprovals','sendUserMessage','interrupt','getApproval','resolveApproval'])
net.createServer(socket=>{
  sockets.add(socket); socket.on('close',()=>sockets.delete(socket))
  createInterface({input:socket}).on('line',async line=>{
    const {id,method,args}=decode(line)
    try {
      if(!methods.has(method)) throw new Error('invalid_argument')
      socket.write(encode({id,value:await (method.endsWith('Credential') ? credentials : adapter)[method](...args)})+'\n')
    } catch(error) {socket.write(encode({id,error:error.code??'internal'})+'\n')}
  })
}).listen(process.env.WHALEREIN_DSH_SOCKET)
createInterface({input:process.stdin}).on('line',async line=>{
  try {
    const {id,method,approval_id,session_id}=JSON.parse(line)
    let value
    if(method==='create_session') value=adapter.createSession({sessionId:session_id})
    else if(method==='request_approval') {adapter.setRunning(session_id,true);value=adapter.requestApproval({sessionId:session_id,toolName:'fixture-tool'})}
    else if(method==='approval') value=await adapter.getApproval(approval_id)
    else if(method==='snapshot') value=await adapter.getSessionSnapshot(session_id)
    else if(method==='running'){adapter.setRunning(session_id,true);value=true}
    else if(method==='preview_start' || method==='preview_chunk') {
      const start=method==='preview_start'
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:start
        ? {stream_id:'fixture-attempt',revision:1,phase:'start',reply_id:'1:1'}
        : {stream_id:'fixture-attempt',revision:2,phase:'chunk',index:0,text:'x'}})
      value=true
    }
    else if(method==='preview_final') {
      const event=adapter.emit(session_id,{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{content:[{type:'text',text:'xy'}]}}})
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'fixture-attempt',revision:3,phase:'end',outcome:'committed',committed_seq:event.seq}})
      value=true
    }
    else if(method==='emit_silent')value=adapter.emit(session_id,{type:'assistant/message',kind:'output',data:{text:'x'},suppressNotification:true})
    else if(method==='emit')value=adapter.output(session_id,'fixture output')
    else throw new Error('unsupported control')
    process.stdout.write(JSON.stringify({id,value})+'\n')
  }catch{process.stdout.write('{"error":"fixture control failed"}\n')}
})
