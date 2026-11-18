import test from 'node:test'
import assert from 'node:assert/strict'
import { SourceAdapter, validateSessionUpdate } from '../src/contracts/source.js'

test('a source without optional capabilities rejects operations explicitly and never exposes a credential interface',async()=>{
 const source=new SourceAdapter(),metadata={get(){return null},set(){}}
 await source.open({metadata});assert.equal(source.metadata,metadata)
 for(const operation of ['listSessions','getSession','readHistory','readState','execute','executeHost','listModels','readSessionModel','readPendingQueue','readWaitState','waitSession','listApprovals','getApproval','listQuestionPage','getQuestion'])await assert.rejects(source[operation](),e=>e.code==='unsupported_capability')
 assert.deepEqual(await source.readQuestionStamp('s'),{availability:'unsupported',code:'unsupported_capability'})
 assert.deepEqual(await source.describeHostCapabilities(),{})
 assert.throws(()=>source.watch(),e=>e.code==='unsupported_capability')
 assert.equal('getCredential' in source,false);await source.close()
})

test('normalized contract validation rejects missing scopes, ambiguous families, malformed intervals and content',()=>{
 const session={session_id:'s'}
 const history={from_cursor:'a',to_cursor:'b',batch_id:'b',changes:[]}
 assert.equal(validateSessionUpdate({session,history}).history,history)
 for(const update of [{history},{session},{session,history,reset:{}},{session,history:{...history,from_cursor:''}},
  {session,history:{...history,changes:[{}]}},{session,history:{...history,changes:[{message:{message_id:'m',role:'ASSISTANT',blocks:[{block_id:'b',text:{},notice:{}}]}}]}},
  {session,live:{attempt:{live_epoch:'e',attempt_id:'a'},first_seq:0,last_seq:1,started:{}}},
  {session,live:{attempt:{live_epoch:'e',attempt_id:'a'},first_seq:1,last_seq:1,delta:{text:'x'}}},
 ])assert.throws(()=>validateSessionUpdate(update),e=>e.code==='invalid_argument')
 const live={attempt:{live_epoch:'e',attempt_id:'a'},block_id:'b',first_seq:1,last_seq:2,delta:{text:'x'}}
 assert.equal(validateSessionUpdate({session,live}).live,live)
})

test('question updates validate availability, stamps and the single-update rule',()=>{
 const session={session_id:'s'}
 const descriptor={availability:'available',stamp:{observation_epoch:'e',revision:2}}
 assert.equal(validateSessionUpdate({session,questions:descriptor}).questions,descriptor)
 for(const invalid of [
  {availability:'QUESTION_AVAILABILITY_UNSPECIFIED',stamp:{observation_epoch:'e',revision:1}},
  {availability:'available'},
  {availability:'available',stamp:{observation_epoch:'',revision:1}},
  {availability:'available',stamp:{observation_epoch:'e',revision:0}},
  {availability:'available',stamp:{observation_epoch:'e',revision:'0'}},
  {availability:'unavailable',code:'x'.repeat(65)},
 ])assert.throws(()=>validateSessionUpdate({session,questions:invalid}),e=>e.code==='invalid_argument')
 assert.throws(()=>validateSessionUpdate({session,questions:{availability:'unsupported'},history:{from_cursor:'a',to_cursor:'b',batch_id:'b',changes:[]}}),e=>e.code==='invalid_argument')
 assert.equal(validateSessionUpdate({session,questions:{availability:'unavailable',code:'unavailable'}}).questions.availability,'unavailable')
})
