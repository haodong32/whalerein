import { MemoryHostCredentials } from '../../../host/runtime/src/security/credentials.js'
// Isolated simulated DSH authority; no production DSH access.
import net from 'node:net'
import { createInterface } from 'node:readline'
import { DefinitiveExecutionError } from '../../../host/runtime/src/errors.js'
import { FakeDshAdapter } from '../../../host/dsh/src/adapter/fake.js'
import { encode,decode } from './adapter.mjs'
const adapter=new FakeDshAdapter({behavior:{modelOperations:true,pendingQueueEnabled:true,questions:process.env.WHALEREIN_QUESTIONS!=='false',questionsOpen:process.env.WHALEREIN_QUESTIONS!=='false'}})
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
const sockets=new Set()
let budgetProbe=false
let budgetAccepted=0
for(const [method,event] of [['subscribeEvents','events'],['subscribeApprovals','approvals']]) {
  adapter[method](value=>{for(const socket of sockets)socket.write(encode({event,value})+'\n')})
}
const methods=new Set(['executeHost','getCredential','setCredential','nativeHead','listSessions','getSessionSnapshot','listApprovals','sendUserMessage','interrupt','getApproval','resolveApproval','listModels','readSessionModel','selectSessionModel','readPendingQueue','updatePendingQueue','forkSession','listQuestionPage','getQuestion','answerContinuedQuestion','answerOpenQuestion','cancelOpenQuestion'])
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
  try {
    const {id,method,method_name,ttl_ms,approval_id,session_id,size,count,attempt_id,index,text,duplicate,stream_id,updates,gap_ms,start_ms,final_ms,delay_ms,duplicate_after_ms,fault,reason,tool_name}=JSON.parse(line)
    let value
    if(method==='fail_next_session_refresh') { adapter.failNext('listSessions'); value=true }
    else if(method==='queue_read_count') value=queueReadCount
    else if(method==='gx_effect_counts') value={...effectCounts}
    else if(method==='enable_budget_probe') {budgetProbe=true;value=true}
    else if(method==='budget_count') value=budgetAccepted
    else if(method==='question_answer_calls') value=questionAnswerCalls
    else if(method==='model_counts') value={...modelCalls}
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
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:start
        ? {stream_id:'fixture-attempt',revision:1,phase:'start',reply_id:'1:1',block_id:'text-0'}
        : {stream_id:'fixture-attempt',revision:2,phase:'chunk',index:0,block_id:'text-0',text:'x'}})
      value=true
    }
    else if(method==='preview_final') {
      const event=adapter.emit(session_id,{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{content:[{type:'text',text:'xy'}]}}})
      adapter.emit(session_id,{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'fixture-attempt',revision:3,phase:'end',block_id:'text-0',outcome:'committed',committed_seq:event.seq}})
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
  }catch{process.stdout.write('{"error":"fixture control failed"}\n')}
})
