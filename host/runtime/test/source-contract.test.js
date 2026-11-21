import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  SourceAdapter,
  validateSessionUpdate,
  validateHistoryRead,
  validateMessageReply,
  validateAttachmentMetadata,
  validateAttachmentPart,
} from '../src/contracts/source.js'
import {
  BROKER_PACKET_LIMIT_BYTES,
  DEFAULT_TRANSPORT_PROFILE,
  TRANSPORT_PROFILES,
  TRANSPORT_PROFILES_SCHEMA,
  TRANSPORT_PROFILE_WIRE_NAMES,
  resolveTransportProfile,
  transportProfileBudget,
  validateReceiveAdvertisement,
  validateRequestTransportMetadata,
  validateTransportBudget,
  validateTransportProfiles,
} from '../src/contracts/transport.js'
import { ProtoCodec } from '../src/protocol.js'

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
 const live={attempt:{live_epoch:'e',attempt_id:'a'},block_id:'b',first_seq:1,last_seq:2,delta:{kind:'BLOCK_KIND_TEXT',text:'x'}}
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

test('typed reasoning and live block kinds stay on their own identity',()=>{
 const session={session_id:'s'}
 const live=(payload)=>({session,live:{attempt:{live_epoch:'e',attempt_id:'a'},block_id:'b',first_seq:1,last_seq:1,...payload}})
 assert.equal(validateSessionUpdate(live({delta:{kind:'BLOCK_KIND_REASONING',text:'think'}})).live.delta.kind,'BLOCK_KIND_REASONING')
 assert.equal(validateSessionUpdate(live({reset:{kind:'BLOCK_KIND_TEXT',text:'answer'}})).live.reset.kind,'BLOCK_KIND_TEXT')
 for(const payload of [
  {delta:{text:'missing kind'}},
  {delta:{kind:'BLOCK_KIND_UNSPECIFIED',text:'x'}},
  {delta:{kind:'BLOCK_KIND_TOOL_ACTIVITY',text:'x'}},
  {reset:{kind:'',text:'x'}},
 ])assert.throws(()=>validateSessionUpdate(live(payload)),e=>e.code==='invalid_argument')
})

test('tool activity, notices and context authorship validate strictly',()=>{
 const session={session_id:'s'}
 const activity={call_id:'call-1',stages:['TOOL_STAGE_PREPARATION','TOOL_STAGE_EXECUTION','TOOL_STAGE_SETTLEMENT'],name:'read_file',summary:'Read a file',result:[{text:{text:'ok'}},{attachment:{media_type:'text/plain',name:'note.txt',size:4,available:true,attachment_ref:'occurrence-1'}}],outcome:'TOOL_OUTCOME_FAILED',failure_code:'file_unreadable'}
 const history=(message)=>({session,history:{from_cursor:'a',to_cursor:'b',batch_id:'b',projection_id:'p',changes:[{message}]}})
 assert.equal(validateSessionUpdate(history({message_id:'m',role:'TOOL',blocks:[{block_id:'tool',tool_activity:activity}]})).history.changes[0].message.blocks[0].tool_activity.call_id,'call-1')
 assert.equal(validateSessionUpdate(history({message_id:'m',role:'NOTICE',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_RETRY',state:'NOTICE_STATE_SCHEDULED',summary:'Retry scheduled'}}]})).history.changes[0].message.role,'NOTICE')
 assert.equal(validateSessionUpdate(history({message_id:'m',role:'MESSAGE_ROLE_CONTEXT',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_CONTEXT',state:'NOTICE_STATE_COMPLETED'}}]})).history.changes[0].message.role,'MESSAGE_ROLE_CONTEXT')
 assert.equal(validateSessionUpdate(history({message_id:'m',role:'MESSAGE_ROLE_UNKNOWN',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_OMISSION',state:'NOTICE_STATE_COMPLETED'}}]})).history.changes[0].message.role,'MESSAGE_ROLE_UNKNOWN')
 assert.equal(validateSessionUpdate(history({message_id:'m',role:'ASSISTANT',blocks:[{block_id:'r',reasoning:{text:'think'}},{block_id:'t',text:{text:'answer'}}]})).history.changes[0].message.blocks.length,2)
 for(const message of [
  {message_id:'m',role:'MESSAGE_ROLE_UNSPECIFIED',blocks:[{block_id:'t',text:{text:'x'}}]},
  {message_id:'m',role:'USER',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_CONTEXT',state:'NOTICE_STATE_COMPLETED'}}]},
  {message_id:'m',role:'MESSAGE_ROLE_CONTEXT',blocks:[{block_id:'t',text:{text:'private context'}}]},
  {message_id:'m',role:'MESSAGE_ROLE_UNKNOWN',blocks:[{block_id:'a',attachment:{media_type:'text/plain',name:'x',size:1,available:true,attachment_ref:'o'}}]},
  {message_id:'m',role:'TOOL',blocks:[{block_id:'tool',tool_activity:{...activity,stages:['TOOL_STAGE_EXECUTION']}}]},
  {message_id:'m',role:'TOOL',blocks:[{block_id:'tool',tool_activity:{...activity,stages:['TOOL_STAGE_EXECUTION'],outcome:'TOOL_OUTCOME_SUCCEEDED'}}]},
  {message_id:'m',role:'TOOL',blocks:[{block_id:'tool',tool_activity:{...activity,failure_code:undefined}}]},
  {message_id:'m',role:'NOTICE',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_UNSPECIFIED',state:'NOTICE_STATE_COMPLETED'}}]},
  {message_id:'m',role:'NOTICE',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_RETRY',state:'NOTICE_STATE_UNSPECIFIED'}}]},
  {message_id:'m',role:'NOTICE',blocks:[{block_id:'n',notice:{category:'NOTICE_CATEGORY_RETRY',state:'NOTICE_STATE_COMPLETED',text:'raw'}}]},
 ])assert.throws(()=>validateSessionUpdate(history(message)),e=>e.code==='invalid_argument')
})

test('history selection modes are exclusive and continuation carries scope',()=>{
 const exact={session:{host_id:'h',session_id:'s'},message_id:'m',source_locator:'locator',projection_id:'p'}
 assert.equal(validateHistoryRead({session_id:'s',page:{before_cursor:'opaque'},cut_cursor:'opaque',max_bytes:1024,max_messages:1}).page.before_cursor,'opaque')
 assert.equal(validateHistoryRead({session_id:'s',exact}).exact.message_id,'m')
 assert.equal(validateHistoryRead({session_id:'s',after_cursor:'a',source:'HISTORY_SOURCE_ONLINE'}).after_cursor,'a')
 assert.equal(validateHistoryRead({session_id:'s',continuation:'core-owned'}).continuation,'core-owned')
 for(const read of [
  {session_id:'s',page:{before_cursor:'a'},exact},
  {session_id:'s',after_cursor:'a',page:{}},
  {session_id:'s',continuation:'c',exact},
  {session_id:'s',page:{before_cursor:'a'},continuation:'c'},
  {session_id:'s',source:'HISTORY_SOURCE_UNSPECIFIED'},
  {session_id:'s',exact:{...exact,projection_id:''}},
 ])assert.throws(()=>validateHistoryRead(read),e=>e.code==='invalid_argument')
})

test('typed send replies separate pending, committed, input-required and uncertain',()=>{
 const committed={session:{host_id:'h',session_id:'s'},message_id:'m',source_locator:'locator',projection_id:'p'}
 assert.equal(validateMessageReply({session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_COMMITTED',committed}).state,'MESSAGE_REPLY_COMMITTED')
 assert.deepEqual(validateMessageReply({session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_INPUT_REQUIRED',pending_input:['INPUT_APPROVAL','INPUT_QUESTION']}).pending_input,['INPUT_APPROVAL','INPUT_QUESTION'])
 assert.equal(validateMessageReply({session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_PENDING'}).state,'MESSAGE_REPLY_PENDING')
 assert.equal(validateMessageReply({session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_UNCERTAIN'}).state,'MESSAGE_REPLY_UNCERTAIN')
 for(const reply of [
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_STATE_UNSPECIFIED'},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_COMMITTED'},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_COMMITTED',committed,pending_input:['INPUT_OTHER']},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_INPUT_REQUIRED'},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_INPUT_REQUIRED',pending_input:['INPUT_KIND_UNSPECIFIED']},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_INPUT_REQUIRED',pending_input:['INPUT_APPROVAL','INPUT_APPROVAL']},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_PENDING',committed},
  {session_id:'s',submitted_message_id:'submitted',state:'MESSAGE_REPLY_UNCERTAIN',pending_input:['INPUT_OTHER']},
 ])assert.throws(()=>validateMessageReply(reply),e=>e.code==='invalid_argument')
})

test('attachment metadata and parts enforce bounded scope and terminal integrity',()=>{
 const metadata={session:{host_id:'h',session_id:'s'},attachment_ref:'occurrence-1',media_type:'text/plain',name:'note.txt',size:6,policy:{max_total_bytes:1048576,max_chunk_bytes:61440,max_concurrent:2,idle_timeout_ms:30000,overall_timeout_ms:120000},transfer_id:'transfer-1',expires_at_unix_ms:1700000000000}
 assert.equal(validateAttachmentMetadata(metadata).transfer_id,'transfer-1')
 const part=(fields)=>({session:{host_id:'h',session_id:'s'},attachment_ref:'occurrence-1',transfer_id:'transfer-1',offset:0,length:2,data:Uint8Array.of(1,2),final:false,...fields})
 assert.equal(validateAttachmentPart(part({}),metadata).length,2)
 assert.equal(validateAttachmentPart(part({offset:4,length:2,data:Uint8Array.of(5,6),final:true,total_size:6,sha256:new Uint8Array(32)}),metadata).final,true)
 for(const fields of [
  {offset:0,length:2,data:Uint8Array.of(1)},
  {offset:0,length:2,data:Uint8Array.of(1,2),final:true,total_size:3,sha256:new Uint8Array(32)},
  {offset:0,length:2,data:Uint8Array.of(1,2),final:true,total_size:2,sha256:new Uint8Array(31)},
  {offset:0,length:2,data:Uint8Array.of(1,2),final:false,total_size:2},
  {offset:5,length:2,data:Uint8Array.of(1,2)},
 ])assert.throws(()=>validateAttachmentPart(part(fields),metadata),e=>['invalid_argument','payload_too_large'].includes(e.code))
 for(const bad of [
  {...metadata,attachment_ref:''},
  {...metadata,size:1048577},
  {...metadata,transfer_id:''},
  {...metadata,policy:{...metadata.policy,max_chunk_bytes:0}},
  {...metadata,policy:{...metadata.policy,max_chunk_bytes:1048577}},
 ])assert.throws(()=>validateAttachmentMetadata(bad),e=>['invalid_argument','payload_too_large'].includes(e.code))
})

test('packaged transport profiles and receive advertisements validate together',()=>{
 assert.equal(transportProfileBudget('default').packet_bytes,65536)
 assert.equal(transportProfileBudget('default').envelope_bytes,16777216)
 assert.equal(transportProfileBudget('compact').packet_bytes,32768)
 assert.equal(transportProfileBudget('roomy').packet_bytes,131072)
 assert.equal(validateTransportBudget(transportProfileBudget('compact')).bulk_concurrency,2)
 assert.equal(
  validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget:transportProfileBudget('default'),connection_generation:3,relay_version:3}).connection_generation,
  3,
 )
 // The protobuf codec decodes 64-bit integers as decimal strings.
 {
  const strings = { ...transportProfileBudget('default') }
  for (const key of ['envelope_bytes','reserved_bytes']) strings[key] = String(strings[key])
  const advertisement = validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget:strings,connection_generation:'9',relay_version:3})
  assert.equal(advertisement.connection_generation,9)
  assert.equal(advertisement.budget.envelope_bytes,16777216)
  assert.equal(advertisement.relay_version,3)
  assert.deepEqual(validateRequestTransportMetadata({result_max_bytes:'25165824',grant_generation:'7',revocation_generation:'3'}),{result_max_bytes:25165824,grant_generation:7,revocation_generation:3})
 }
 assert.deepEqual(validateRequestTransportMetadata({result_max_bytes:16777216,grant_generation:7,revocation_generation:2}),{result_max_bytes:16777216,grant_generation:7,revocation_generation:2})
 assert.equal(TRANSPORT_PROFILES_SCHEMA,'whalerein/transport-profiles/v1')
 assert.equal(DEFAULT_TRANSPORT_PROFILE,'default')
 assert.deepEqual(TRANSPORT_PROFILE_WIRE_NAMES,{default:'TRANSPORT_PROFILE_DEFAULT',compact:'TRANSPORT_PROFILE_COMPACT',roomy:'TRANSPORT_PROFILE_ROOMY'})
 assert.doesNotThrow(()=>validateTransportProfiles())
 for(const profile of ['TRANSPORT_PROFILE_UNSPECIFIED','TRANSPORT_PROFILE_FUTURE','unknown'])assert.throws(()=>resolveTransportProfile(profile),e=>e.code==='invalid_argument')
 for(const budget of [
  {...transportProfileBudget('default'),envelope_bytes:1},
  {...transportProfileBudget('default'),packet_bytes:0},
  {...transportProfileBudget('default'),packet_bytes:1048577},
  {...transportProfileBudget('default'),bulk_concurrency:0},
  {...transportProfileBudget('default'),reserved_bytes:1},
  {...transportProfileBudget('default'),transfer_timeout_ms:0},
 ])assert.throws(()=>validateTransportBudget(budget),e=>e.code==='invalid_argument')
 assert.throws(()=>validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget:transportProfileBudget('default'),connection_generation:0,relay_version:3}),e=>e.code==='invalid_argument')
 assert.throws(()=>validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget:transportProfileBudget('default'),connection_generation:1}),e=>e.code==='protocol_version_incompatible')
 assert.throws(()=>validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget:transportProfileBudget('default'),connection_generation:1,relay_version:2}),e=>e.code==='protocol_version_incompatible')
 assert.throws(()=>validateRequestTransportMetadata({result_max_bytes:0,grant_generation:1,revocation_generation:1}),e=>e.code==='invalid_argument')
 assert.throws(()=>validateRequestTransportMetadata({result_max_bytes:1,grant_generation:-1,revocation_generation:1}),e=>e.code==='invalid_argument')
 assert.deepEqual(validateRequestTransportMetadata({result_max_bytes:1,grant_generation:1}),{result_max_bytes:1,grant_generation:1,revocation_generation:0})
 assert.deepEqual(validateRequestTransportMetadata({result_max_bytes:'1'}),{result_max_bytes:1,grant_generation:0,revocation_generation:0})
 assert.throws(()=>validateRequestTransportMetadata({result_max_bytes:1,grant_generation:1,revocation_generation:-1}),e=>e.code==='invalid_argument')
})

test('a relation-valid receive budget override is accepted under a known profile label and never matches a packaged profile',()=>{
 // The budget may carry directional operator overrides; it is returned
 // normalized and unclamped so the sender can take the minimum.
 const unpackaged = {
  ...transportProfileBudget('default'),
  packet_bytes: 32768,
  envelope_bytes: 4 * 1024 * 1024,
  reserved_bytes: 8 * 1024 * 1024,
 }
 const accepted=validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_COMPACT',budget:unpackaged,connection_generation:Number.MAX_SAFE_INTEGER,relay_version:3})
 assert.equal(accepted.profile,'TRANSPORT_PROFILE_COMPACT')
 assert.equal(accepted.connection_generation,Number.MAX_SAFE_INTEGER)
 assert.deepEqual(accepted.budget,unpackaged)
 assert.ok(Object.isFrozen(accepted.budget))
 assert.ok(Object.isFrozen(accepted))
 const contradictory=[
  {...unpackaged,packet_bytes:0},
  {...unpackaged,packet_bytes:1048577},
  {...unpackaged,envelope_bytes:32767},
  {...unpackaged,envelope_bytes:0},
  {...unpackaged,reserved_bytes:1},
  {...unpackaged,bulk_concurrency:0},
  {...unpackaged,transfer_timeout_ms:0},
  {...unpackaged,reserved_bytes:-1},
 ]
 for(const budget of contradictory)assert.throws(()=>validateReceiveAdvertisement({profile:'TRANSPORT_PROFILE_DEFAULT',budget,connection_generation:1,relay_version:3}),e=>e.code==='invalid_argument')
 for(const advertisement of [
  null,
  {profile:'TRANSPORT_PROFILE_DEFAULT',connection_generation:1,relay_version:3},
  {profile:'TRANSPORT_PROFILE_FUTURE',budget:unpackaged,connection_generation:1,relay_version:3},
  {profile:'TRANSPORT_PROFILE_DEFAULT',budget:unpackaged,connection_generation:0,relay_version:3},
  {profile:'TRANSPORT_PROFILE_DEFAULT',budget:unpackaged,connection_generation:-1,relay_version:3},
  {profile:'TRANSPORT_PROFILE_DEFAULT',budget:unpackaged,connection_generation:1.5,relay_version:3},
  {profile:'TRANSPORT_PROFILE_DEFAULT',budget:unpackaged,connection_generation:Number.MAX_SAFE_INTEGER+1,relay_version:3},
 ])assert.throws(()=>validateReceiveAdvertisement(advertisement),e=>e.code==='invalid_argument')
})

test('packaged transport profiles match the executable contract copy',async()=>{
 const { readFileSync } = await import('node:fs')
 const file = JSON.parse(readFileSync(new URL('../../../proto/v1/transport_profiles.json', import.meta.url), 'utf8'))
 assert.equal(file.schema,'whalerein/transport-profiles/v1')
 assert.equal(file.defaultProfile,'default')
 assert.deepEqual(Object.keys(file.profiles).sort(),['compact','default','roomy'])
 for(const [name,profile] of Object.entries(file.profiles)){
  const executable=transportProfileBudget(name)
  assert.equal(profile.wireName,resolveTransportProfile(name).wireName)
  assert.deepEqual(TRANSPORT_PROFILES[name],profile)
  for(const [field,value] of Object.entries(profile))assert.equal(TRANSPORT_PROFILES[name][field],value,`${name}.${field}`)
  assert.equal(profile.packetBytes,executable.packet_bytes)
  assert.equal(profile.envelopeBytes,executable.envelope_bytes)
  assert.equal(profile.bulkConcurrency,executable.bulk_concurrency)
  assert.equal(profile.reservedBytes,executable.reserved_bytes)
  assert.equal(profile.transferTimeoutMs,executable.transfer_timeout_ms)
  assert.equal(file.brokerPacketLimitBytes,BROKER_PACKET_LIMIT_BYTES)
  assert.ok(profile.packetBytes<=BROKER_PACKET_LIMIT_BYTES,`${name}.packetBytes exceeds the broker limit`)
 }
})

test('retired ContentBlock tag 3 decodes as an unknown field, not the old tool shape',()=>{
 const vectors=JSON.parse(readFileSync(new URL('../../../proto/v1/compat/vectors.json',import.meta.url),'utf8')).vectors
 const vector=vectors.find(entry=>entry.id==='content-block-retired-tag-is-ignored')
 assert.ok(vector,'missing ContentBlock retirement vector')
 const codec=new ProtoCodec()
 const decoded=codec.decode('whalerein.v1.ContentBlock',Buffer.from(vector.hex,'hex'))
 assert.equal(decoded.block_id,'b')
 assert.equal(decoded.text.text,'x')
 assert.equal(decoded.tool_activity,undefined)
})
