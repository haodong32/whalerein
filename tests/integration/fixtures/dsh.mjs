import { MemoryHostCredentials } from '../../../host/runtime/src/security/credentials.js'
// Isolated simulated DSH authority; no production DSH access.
import { createHash, randomUUID } from 'node:crypto'
import net from 'node:net'
import { createInterface } from 'node:readline'
import { DefinitiveExecutionError } from '../../../host/runtime/src/errors.js'
import { FakeDshAdapter } from '../../../host/dsh/src/adapter/fake.js'
import { encode,decode } from './adapter.mjs'
const adapter=new FakeDshAdapter({hostId:process.env.WHALEREIN_HOST_ID||'',behavior:{modelOperations:true,pendingQueueEnabled:true,questions:process.env.WHALEREIN_QUESTIONS!=='false',questionsOpen:process.env.WHALEREIN_QUESTIONS!=='false'}})
let queueReadCount=0
const readQueue=adapter.readPendingQueue.bind(adapter)
adapter.readPendingQueue=(...args)=>{queueReadCount++;return readQueue(...args)}
// Benchmark script: the fixture drives its own reply from acceptance of the
// measured CLI send, on fixed relative times. Nothing here reads CLI output, so
// the observer can neither advance nor delay the schedule.
const benchSends=new Map()
const benchArmed=new Map()
const acceptance=session=>{
  benchSends.set(session,(benchSends.get(session)??0)+1)
  const plan=benchArmed.get(session)
  if(!plan)return
  benchArmed.delete(session)
  // Resume in a timer phase, outside the send's own call stack. A script
  // failure is reported on stderr; it never changes the process-wide
  // rejection/uncaught behaviour other harness cases rely on.
  setImmediate(()=>{
    runScript(session,plan).catch(error=>process.stderr.write('benchmark script failed: '+(error?.stack??error)+'\n'))
  })
}
const rawSendUserMessage=adapter.sendUserMessage.bind(adapter)
adapter.sendUserMessage=(...args)=>{const result=rawSendUserMessage(...args);acceptance(args[0]);return result}
const emitPreview=(session,stream,revision,frame)=>adapter.emit(session,{
  type:'assistant/preview',kind:'output',ephemeral:true,
  data:{stream_id:stream,revision,reply_id:stream,...frame}})
const emitFinal=(session,stream,text)=>adapter.emit(session,{
  type:'assistant/message',kind:'output',
  data:{reply_id:stream,message:{content:[{type:'text',text}]}}})
const runScript=(session,plan)=>{
  const {stream,updates,gapMs,startMs,finalMs,delayMs,duplicateAfterMs,fault}=plan
  const started=Date.now()
  const at=ms=>new Promise(resolve=>setTimeout(resolve,Math.max(0,started+ms+delayMs-Date.now())))
  let revision=1
  const settle=async text=>{
    const event=emitFinal(session,stream,text)
    emitPreview(session,stream,++revision,{phase:'end',outcome:'committed',committed_seq:event.seq})
    if(duplicateAfterMs!=null){
      await at(finalMs+duplicateAfterMs)
      emitFinal(session,stream,text)
    }
  }
  const script=async()=>{
    if(fault==='silent')return
    emitPreview(session,stream,++revision,{phase:'start'})
    for(let index=0;index<updates.length;index++){
      await at(startMs+index*gapMs)
      emitPreview(session,stream,++revision,{phase:'chunk',index,text:updates[index]})
    }
    if(fault==='partial')return
    await at(finalMs)
    await settle(fault==='incorrect'?'incorrect final reply':updates.join(''))
  }
  return script()
}
const credentials=new MemoryHostCredentials()
const answerControllers=new Map()
let questionAnswerCalls = 0
const answerOpenQuestion = adapter.answerOpenQuestion.bind(adapter)
adapter.answerOpenQuestion = (...args) => { questionAnswerCalls++; return answerOpenQuestion(...args) }
const modelCalls = { listModels: 0, readSessionModel: 0, selectSessionModel: 0 }
for (const method of Object.keys(modelCalls)) {
  const invoke = adapter[method].bind(adapter)
  adapter[method] = (...args) => { modelCalls[method]++; return invoke(...args) }
}
// Count test fixture operations without retaining message or parameter data.
const effectCounts = Object.fromEntries(['executeHost','forkSession','sendUserMessage','interrupt','selectSessionModel','updatePendingQueue','answerOpenQuestion','answerContinuedQuestion','resolveApproval'].map(name => [name,0]))
for (const method of Object.keys(effectCounts)) {
  const original = adapter[method].bind(adapter)
  adapter[method] = (...args) => { effectCounts[method]++; return original(...args) }
}
// --- attachment fixture -------------------------------------------------
// Deterministic bytes live only in this process. The control channel carries a
// seed/size descriptor forward and base64 parts backward; no bytes are logged.
const attachmentReaders=new Map()
let nextAttachmentTransfer=1
let attachmentPulls=0
let attachmentPullBytes=0
let attachmentPullHold=false
let attachmentPullHeld=false
let attachmentPullRelease=null
const deterministicAttachmentBytes=(size, seed)=>{
  const bytes=new Uint8Array(Math.max(0, Number(size)||0))
  for(let index=0;index<bytes.length;index++) bytes[index]=(seed*131+index*17+((index>>8)*7))&0xff
  return bytes
}
const releaseHeldAttachmentPull=()=>{
  const release=attachmentPullRelease
  attachmentPullRelease=null
  attachmentPullHeld=false
  release?.()
}
adapter.read_fixture_image=async(native)=>{
  const data=await adapter.nativeReadImage(native)
  return {data:Buffer.from(data).toString('base64')}
}
adapter.open_fixture_file=async(native)=>{
  const reader=await adapter.nativeReadFileStream(native)
  const transfer=`fixture-transfer-${nextAttachmentTransfer++}`
  attachmentReaders.set(transfer,{reader,pending:null})
  return {transfer}
}
adapter.pull_fixture_file=async(transfer, maxBytes)=>{
  const state=attachmentReaders.get(transfer)
  if(!state){const error=new Error('not_found');error.code='not_found';throw error}
  if(attachmentPullHold&&!attachmentPullHeld){
    attachmentPullHeld=true
    await new Promise(resolve=>{attachmentPullRelease=resolve})
  }
  const chunks=[]
  let produced=0, eof=false
  while(produced<maxBytes){
    if(!state.pending){
      const next=await state.reader.next()
      if(next.done){eof=true;break}
      state.pending=next.value instanceof Uint8Array?next.value:Uint8Array.from(next.value??[])
      if(state.pending.length===0){state.pending=null;continue}
    }
    const take=Math.min(maxBytes-produced,state.pending.length)
    chunks.push(state.pending.subarray(0,take))
    produced+=take
    state.pending=take===state.pending.length?null:state.pending.subarray(take)
  }
  const bytes=new Uint8Array(produced)
  let offset=0
  for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length}
  attachmentPulls++
  attachmentPullBytes+=bytes.length
  return {data:Buffer.from(bytes).toString('base64'), eof}
}
adapter.close_fixture_file=async(transfer)=>{
  // A cancelled transfer must not leave a deliberately-held read unresolved.
  releaseHeldAttachmentPull()
  const state=attachmentReaders.get(transfer)
  attachmentReaders.delete(transfer)
  try{await state?.reader?.return?.()}catch{/* cleanup is best effort */}
  return true
}
const sockets=new Set()
let budgetProbe=false
let budgetAccepted=0
for(const [method,event] of [['subscribeEvents','events'],['subscribeApprovals','approvals']]) {
  adapter[method](value=>{for(const socket of sockets)socket.write(encode({event,value})+'\n')})
}
const methods=new Set(['executeHost','getCredential','setCredential','nativeHead','listSessions','getSessionSnapshot','listApprovals','sendUserMessage','readMessageReply','interrupt','getApproval','resolveApproval','listModels','readSessionModel','selectSessionModel','readPendingQueue','updatePendingQueue','forkSession','readQuestionStamp','listQuestionPage','getQuestion','answerContinuedQuestion','answerOpenQuestion','cancelOpenQuestion','read_fixture_image','open_fixture_file','pull_fixture_file','close_fixture_file'])
net.createServer(socket=>{
  sockets.add(socket); socket.on('close',()=>sockets.delete(socket))
  createInterface({input:socket}).on('line',async line=>{
    const {id,method,args}=decode(line)
    try {
      if(!methods.has(method)) throw new Error('invalid_argument')
      if(method==='sendUserMessage' && budgetProbe) {
        budgetAccepted++
        socket.write(encode({id,value:{messageId:args[3],deliveryId:args[3]}})+'\n')
      } else if(method==='answerOpenQuestion') {
        const [request, operationId]=args, controller=new AbortController()
        answerControllers.set(operationId, controller)
        try { socket.write(encode({id,value:await adapter.answerOpenQuestion(request,controller.signal)})+'\n') }
        finally { answerControllers.delete(operationId) }
      } else if(method==='cancelOpenQuestion') {
        answerControllers.get(args[0])?.abort()
        socket.write(encode({id,value:true})+'\n')
      } else socket.write(encode({id,value:await (method.endsWith('Credential') ? credentials : adapter)[method](...args)})+'\n')
    } catch(error) {socket.write(encode({id,error:error.code??'internal',definitive:error instanceof DefinitiveExecutionError})+'\n')}
  })
}).listen(process.env.WHALEREIN_DSH_SOCKET)
createInterface({input:process.stdin}).on('line',async line=>{
  let id
  try {
    const control=JSON.parse(line)
    id=control.id
    const {method,method_name,ttl_ms,approval_id,session_id,message_id,size,count,attempt_id,index,text,duplicate,stream_id,updates,gap_ms,start_ms,final_ms,delay_ms,duplicate_after_ms,fault,reason,tool_name,attachment_kind,seed,media_type,name,attachment,role,hold}=control
    let value
    if(method==='fail_next_session_refresh') { adapter.failNext('listSessions'); value=true }
    else if(method==='queue_read_count') value=queueReadCount
    else if(method==='gx_effect_counts') value={...effectCounts}
    else if(method==='enable_budget_probe') {budgetProbe=true;value=true}
    else if(method==='budget_count') value=budgetAccepted
    else if(method==='question_answer_calls') value=questionAnswerCalls
    else if(method==='model_counts') value={...modelCalls}
    else if(method==='save_attachment') {
      const bytes=deterministicAttachmentBytes(size,Number(seed)||0)
      const ref=adapter.saveFixtureAttachment({
        kind:attachment_kind==='image'?'image':'file', data:bytes,
        mediaType:typeof media_type==='string'?media_type:'',
        name:typeof name==='string'?name:'',
      })
      // File occurrences normally carry no native media type; keep the
      // declared one visible to history without changing the occurrence id.
      if(typeof media_type==='string'&&media_type!==''&&!ref.mediaType) ref.mediaType=media_type
      value={attachment:ref, bytes:bytes.length, sha256:createHash('sha256').update(bytes).digest('hex')}
    }
    else if(method==='commit_attachment') {
      if(!attachment||typeof attachment!=='object') throw new Error('invalid_argument')
      const blockType=attachment_kind==='image'?'image':'file'
      const messageId='fixture-attachment-'+randomUUID()
      const textValue=typeof text==='string'&&text!==''?text:'fixture attachment'
      // The client-visible occurrence reference belongs to the adapter's local
      // history view. Tests read it from `session history`; the control only
      // proves the committed message was appended.
      const event=role==='tool'
        ? adapter.emit(session_id,{type:'tool/result',kind:'output',data:{message:{id:messageId,role:'tool',content:[{type:'text',text:textValue},{type:blockType,attachment}],source:{callId:'fixture-attachment-call'}}}})
        : adapter.emit(session_id,{type:'user/message',kind:'output',data:{id:messageId,role:'user',content:[{type:'text',text:textValue},{type:blockType,attachment}],source:{kind:'user',rpcId:messageId}}})
      value={seq:event.seq}
    }
    else if(method==='attachment_pull_state') value={waiting:attachmentPullHeld,pulls:attachmentPulls,bytes:attachmentPullBytes}
    else if(method==='set_attachment_pull_hold') { attachmentPullHold=hold!==false; if(!attachmentPullHold) releaseHeldAttachmentPull(); value=true }
    else if(method==='release_attachment_pull') { releaseHeldAttachmentPull(); value=true }
    else if(method==='reset_attachment_stats') { attachmentPulls=0; attachmentPullBytes=0; value=true }
    else if(method==='create_session') value=adapter.createSession({sessionId:session_id})
    else if(method==='create_queue_budget_session') value=adapter.createSession({sessionId:session_id,running:true,queue:[{id:'queue-budget-1',text:'first'},{id:'queue-budget-2',text:'second'}]})
    else if(method==='set_queue_budget_size') { if(!Number.isSafeInteger(size)||size<1||size>17*1024*1024) throw new Error('invalid_argument'); await adapter.updatePendingQueue(session_id,{item_id:'queue-budget-1',action:'EDIT',text:'x'.repeat(size)}); value=true }
    else if(method==='queue_budget_state') { const queue=await adapter.readPendingQueue(session_id);value=queue.items.map(item=>({id:item.item_id,length:item.blocks.reduce((sum,block)=>sum+(block.text?.text?.length ?? 0),0)})) }
    else if(method==='create_queue_session') value=adapter.createSession({sessionId:session_id,running:true,queue:[{id:'queue-fixture',text:'fixture'}]})
    else if(method==='create_cold_session') value=adapter.createSession({sessionId:session_id,agentAvailable:false})
    else if(method==='set_queue_idle') { adapter.setRunning(session_id,false); value=true }
    else if(method==='set_model_failures') { adapter.behavior.modelFailures=Array.isArray(text)?text.map(id=>({id,name:id})):[]; value=true }
    else if(method==='set_questions_enabled') { adapter.behavior.questions=text!==false; adapter.behavior.questionsOpen=text!==false; value=true }
    else if(method==='request_question') { adapter.setFixtureQuestion(session_id,{callId:'fixture-call',state:'continued',questions:[{id:'q1',question:'Fixture question',options:[{label:'Yes'},{label:'No'}]}]}); value=true }
    else if(method==='request_question_open') { adapter.setFixtureQuestion(session_id,{callId:'fixture-call',state:'open',questions:[{id:'q1',question:'Fixture question',options:[{label:'Yes'},{label:'No'}]}]}); value=true }
    else if(method==='request_question_pages') { adapter.setFixtureQuestions(session_id,['first','second'].map(callId => ({callId,state:'continued',questions:[{id:'q1',question:'x'.repeat(9*1024*1024)}]}))); value=true }
    else if(method==='request_question_custom') { adapter.setFixtureQuestion(session_id,{callId:'custom-call',state:'continued',questions:[{id:'q1',question:text}]}); value=true }
    else if(method==='request_question_unanswerable') { adapter.setFixtureQuestion(session_id,{callId:'answered-call',state:'reply_queued',questions:[{id:'q1',question:'Fixture answered question'}]}); value=true }
    else if(method==='append_question_page') { adapter.appendFixtureQuestion(session_id,{callId:text,state:'continued',questions:[{id:'q1',question:'x'.repeat(9*1024*1024)}]}); value=true }
    else if(method==='remove_question_page') { adapter.removeFixtureQuestion(session_id,text); value=true }
    else if(method==='request_question_oversized') { adapter.setFixtureQuestion(session_id,{callId:'oversized',state:'continued',questions:[{id:'q1',question:'x'.repeat(17*1024*1024)}]}); value=true }
    else if(method==='hold_question_answer') { adapter.holdFixtureQuestion(session_id); value=true }
    else if(method==='release_question_answer') { adapter.releaseFixtureQuestionHold(session_id); value=true }
    else if(method==='question_answer_waiting') value=adapter.fixtureQuestionWaiting(session_id)
    else if(method==='request_approval') {adapter.setRunning(session_id,true);value=adapter.requestApproval({sessionId:session_id,toolName:typeof tool_name==='string'&&tool_name!==''?tool_name:'fixture-tool',...(typeof reason==='string'?{reason}:{}),...(Number.isSafeInteger(ttl_ms)&&ttl_ms>0?{ttlMs:ttl_ms}:{})})}
    else if(method==='hang_next') { if(typeof method_name!=='string'||!method_name) throw new Error('invalid_argument'); adapter.hangNext(method_name); value=true }
    else if(method==='approval') value=await adapter.getApproval(approval_id)
    else if(method==='forget_approval') value=adapter.forgetApproval(approval_id)
    else if(method==='snapshot') value=await adapter.getSessionSnapshot(session_id)
    else if(method==='running'){adapter.setRunning(session_id,true);value=true}
    else if(method==='idle'){adapter.setRunning(session_id,false);value=true}
    else if(method==='preview_start' || method==='preview_chunk') {
      const start=method==='preview_start'
      const chunkIndex=index??0
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:start
        ? {stream_id:'fixture-attempt',revision:1,phase:'start',reply_id:'1:1',block_id:'text-0'}
        : {stream_id:'fixture-attempt',revision:2+chunkIndex,phase:'chunk',index:chunkIndex,block_id:'text-0',block_kind:'BLOCK_KIND_TEXT',text:text??'x'}})
      value=true
    }
    else if(method==='preview_final') {
      const reply=text??'xy'
      const event=adapter.emit(session_id,{type:'assistant/message',kind:'output',data:{
        reply_id:'1:1',message:{content:[{type:'text',text:reply}]},
        ...(message_id ? {messageId:message_id,text:reply} : {}),
      }})
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'fixture-attempt',revision:2+(count??1),phase:'end',block_id:'text-0',outcome:'committed',native_commit_seq:event.seq}})
      value=true
    }
    else if(method==='emit_silent')value=adapter.emit(session_id,{type:'assistant/message',kind:'output',data:{text:'x'},suppressNotification:true})
    else if(method==='emit')value=adapter.output(session_id,'fixture output')
    // Node's internal `send` command: acceptance is counted by the wrapper above,
    // then the armed schedule resumes in a timer phase without polling. The
    // fixture mirror reply is suppressed so the script is the only reply source.
    else if(method==='bench_schedule') {
      // The scripted reply is the only reply source for a measured attempt. The
      // fixture's immediate echo would commit out of script order and make the
      // watch observe a history gap.
      adapter.behavior.emitOutputOnMessage=false
      benchArmed.set(session_id,{stream:stream_id,updates,gapMs:gap_ms,startMs:start_ms,finalMs:final_ms,delayMs:delay_ms,duplicateAfterMs:duplicate_after_ms,fault})
      value=true
    }
    else if(method==='bench_send_count') value=benchSends.get(session_id)??0
    else throw new Error('unsupported control')
    process.stdout.write(JSON.stringify({id,value})+'\n')
  }catch(error){
    // Surface the stable fixture reason with the request id so a control call
    // fails immediately instead of waiting for the timeout. Attachment bytes
    // are never part of these messages.
    process.stdout.write(JSON.stringify({id,error:typeof error?.message==='string'&&error.message!==''?error.message:'fixture control failed'})+'\n')
  }
})
