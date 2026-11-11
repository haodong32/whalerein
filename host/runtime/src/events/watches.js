// Channel-scoped observation. Source data is already normalized; this module
// owns only watch lifetime, recipients, encryption and bounded publication.
import { LiveBuffer } from './live-buffer.js'
import { randomUUID } from 'node:crypto'
import { PluginError } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { validateSessionUpdate } from '../contracts/source.js'

const LEASE=30000
export class WatchedEvents {
  #watches=new Map()
  #sources=new Map()
  #lanes=new Map()
  #timer=null
  constructor({hostId,store,security,adapter,access,codec=defaultCodec,now=Date.now,config={},logger}) {
    Object.assign(this,{hostId,store,security,adapter,access,now,config,logger});this.codec=resolveCodec(codec)
  }
  start() {this.#timer=setInterval(()=>this.#expire(),1000);this.#timer.unref?.();return this}
  async stop() {
    clearInterval(this.#timer);this.#timer=null
    for(const id of [...this.#watches.keys()])this.#remove(id)
    await Promise.all([...this.#lanes.values()].flatMap(l=>[l.live,l.committed]))
    this.#lanes.clear()
  }
  #expire() {for(const [id,w] of this.#watches)if(w.expires<=this.now()||!this.security.getChannel(w.channelId))this.#remove(id)}
  #remove(id) {
    const watch=this.#watches.get(id);if(!watch)return
    this.#watches.delete(id)
    const source=this.#sources.get(watch.sessionId)
    source?.recipients.delete(id)
    if(source&&!source.recipients.size){source.buffer?.flush();source.subscription?.close();this.#sources.delete(watch.sessionId)}
  }
  #owned(deviceId,channelId,id) {
    this.#expire();const watch=this.#watches.get(id)
    if(!watch||watch.deviceId!==deviceId||watch.channelId!==channelId)throw new PluginError('expired','watch unavailable')
    return watch
  }
  async watch(deviceId,channelId,sessionId) {
    this.#expire()
    const channel=this.security.getChannel(channelId)
    if(!channel||channel.deviceId!==deviceId)throw new PluginError('unauthorized','channel unavailable')
    let watch=[...this.#watches.values()].find(w=>w.deviceId===deviceId&&w.channelId===channelId&&w.sessionId===sessionId)
    if(!watch) {
      if(this.#watches.size>=512||[...this.#watches.values()].filter(w=>w.deviceId===deviceId).length>=8)throw new PluginError('rate_limited','watch budget exceeded')
      if(!this.#sources.has(sessionId)&&this.#sources.size>=64)throw new PluginError('rate_limited','source watch budget exceeded')
      watch={id:randomUUID(),deviceId,channelId,sessionId,expires:this.now()+LEASE}
      this.#watches.set(watch.id,watch)
      let source=this.#sources.get(sessionId)
      if(!source) {
        source={recipients:new Set(),buffer:new LiveBuffer(update=>this.#fanout(sessionId,update))};this.#sources.set(sessionId,source)
        source.recipients.add(watch.id)
        try {
          const subscription=await this.adapter.watch(sessionId,update=>this.#receive(sessionId,update))
          if(this.#sources.get(sessionId)!==source || !source.recipients.size) {
            subscription.close()
            throw new PluginError('expired','watch cancelled during source registration')
          }
          source.subscription=subscription
        }
        catch(error){this.#remove(watch.id);throw error}
      } else source.recipients.add(watch.id)
    }
    try {return await this.#ready(watch)}catch(error){this.#remove(watch.id);throw error}
  }
  async #ready(watch) {
    const state=await this.adapter.readState(watch.sessionId)
    if(state.session?.session_id!==watch.sessionId||!state.history_head||!state.live_epoch)throw new PluginError('unavailable','source state unavailable')
    if(!this.#watches.has(watch.id)||!this.security.getChannel(watch.channelId))throw new PluginError('expired','watch expired during state read')
    state.session={host_id:this.hostId,session_id:watch.sessionId}
    watch.expires=this.now()+LEASE
    return {watch_id:watch.id,state,lease_ms:LEASE}
  }
  async refresh(deviceId,channelId,ids) {
    if(!Array.isArray(ids)||ids.length>8||new Set(ids).size!==ids.length)throw new PluginError('invalid_argument','invalid watch selection')
    return {watches:await Promise.all(ids.map(id=>this.#ready(this.#owned(deviceId,channelId,id))))}
  }
  unwatch(deviceId,channelId,id) {this.#owned(deviceId,channelId,id);this.#remove(id)}
  async readHistory(sessionId,read) {
    const page=await this.adapter.readHistory(sessionId,read)
    if(page.session?.session_id!==sessionId)throw new PluginError('invalid_argument','source history scope mismatch')
    page.session={host_id:this.hostId,session_id:sessionId}
    validateSessionUpdate({session:page.session,history:page.batch})
    return page
  }
  #receive(sessionId,notification) {
    if(notification.state_changed) {
      // Notifications are hints. Native state is read authoritatively and also
      // reconciled on RefreshWatch if a final notification was lost.
      const source=this.#sources.get(sessionId)
      if(!source)return
      if(!source.stateTask) {
        source.stateTask=this.adapter.readState(sessionId).then(state=>{
          if(this.#sources.get(sessionId)!==source)return
          if(state.session?.session_id!==sessionId)throw new PluginError('invalid_argument')
          state.session={host_id:this.hostId,session_id:sessionId}
          this.#receive(sessionId,{session:{session_id:sessionId},state})
        }).catch(()=>this.logger?.warn('source_state_unavailable',{code:'unavailable'})).finally(()=>{source.stateTask=null})
      }
      return
    }
    try {
      if(notification.session?.session_id!==sessionId)throw new PluginError('invalid_argument','source session mismatch')
      const update=validateSessionUpdate({...notification,session:{host_id:this.hostId,session_id:sessionId}})
      this.#expire()
      this.#sources.get(sessionId)?.buffer.push(update)
    } catch {this.logger?.warn('source_update_rejected',{code:'invalid_argument'})}
  }
  #fanout(sessionId,update) {
    for(const id of this.#sources.get(sessionId)?.recipients??[]) {
      const watch=this.#watches.get(id);if(watch)this.#enqueue(watch,{...update,watch_id:id})
    }
  }
  #enqueue(watch,update) {
    const channel=this.security.getChannel(watch.channelId);if(!channel)return
    const plaintext=this.codec.encode('whalerein.v1.EventPayload',{update})
    if(update.live&&plaintext.length>256*1024)return
    const at=this.now()
    const sealed=this.security.sealEvent({deviceId:watch.deviceId,channelId:watch.channelId,plaintext,createdAt:at})
    if(!sealed)return
    const envelope=this.codec.encode('whalerein.v1.EventEnvelope',{host_id:this.hostId,device_id:watch.deviceId,channel_id:watch.channelId,nonce:sealed.nonce,ciphertext:sealed.ciphertext,created_at_unix_ms:at})
    let lane=this.#lanes.get(watch.deviceId)
    if(!lane){lane={live:Promise.resolve(),committed:Promise.resolve(),bytes:0,count:0};this.#lanes.set(watch.deviceId,lane)}
    if(update.live) {
      if(!this.access?.connected||lane.count>=64||lane.bytes+envelope.length>256*1024)return
      lane.count++;lane.bytes+=envelope.length
      lane.live=lane.live.then(()=>{if(this.now()-at<=1000&&this.#watches.has(watch.id)&&this.security.getChannel(watch.channelId)&&this.access.connected)return this.access.publishEvent(envelope)}).catch(()=>{}).finally(()=>{lane.count--;lane.bytes-=envelope.length})
    } else {
      const row=this.store.addOutbox({deviceId:watch.deviceId,channelId:watch.channelId,payload:envelope,now:at,perDeviceLimit:this.config.outboxPerDeviceLimit??100,totalLimit:this.config.outboxTotalLimit??1000})
      if(row.overflow){this.logger?.warn('event_outbox_full',{code:'snapshot_required'});return}
      if(this.access?.connected)lane.committed=lane.committed.then(async()=>{if(!this.#watches.has(watch.id))return;await this.access.publishEvent(envelope);this.store.removeOutbox(row.id)}).catch(()=>this.logger?.warn('event_publish_failed',{code:'unavailable'}))
    }
  }
  async flushOutbox() {
    if(!this.access?.connected)return 0
    let count=0
    for(const row of this.store.listOutbox(this.config.outboxTotalLimit??1000)) {
      if(!this.security.getChannel(row.channelId)){this.store.removeOutbox(row.id);continue}
      try {await this.access.publishEvent(row.payload);this.store.removeOutbox(row.id);count++}catch{break}
    }
    return count
  }
  onChannelReplaced(deviceId,channelId) {
    for(const [id,w] of this.#watches)if(w.deviceId===deviceId&&w.channelId===channelId)this.#remove(id)
    this.store.deleteOutboxForChannel(deviceId,channelId)
    this.#lanes.delete(deviceId)
  }
  onChannelsCleared(channels) {for(const {deviceId,channelId} of channels??[])this.onChannelReplaced(deviceId,channelId)}
  describe(){return {watches:this.#watches.size,sessions:this.#sources.size,pending:this.store.countOutbox()}}
}
