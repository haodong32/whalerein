// Coalescing normalized text updates preserves their inclusive source interval.
export class LiveBuffer {
  #pending=null
  #timer=null
  constructor(publish,delayMs=20){this.publish=publish;this.delayMs=delayMs}
  push(update) {
    const frame=update.live
    if(!frame?.delta){this.flush();this.publish(update);return}
    const previous=this.#pending?.live
    if(previous&&(previous.attempt?.live_epoch!==frame.attempt?.live_epoch||previous.attempt?.attempt_id!==frame.attempt?.attempt_id||previous.block_id!==frame.block_id||previous.last_seq+1!==frame.first_seq||Buffer.byteLength(previous.delta.text)+Buffer.byteLength(frame.delta.text)>8192))this.flush()
    if(!this.#pending){this.#pending={...update,live:{...frame,delta:{...frame.delta}}};this.#timer=setTimeout(()=>this.flush(),this.delayMs);this.#timer.unref?.()}
    else{this.#pending.live.delta.text+=frame.delta.text;this.#pending.live.last_seq=frame.last_seq}
    if(Buffer.byteLength(this.#pending.live.delta.text)>=8192)this.flush()
  }
  flush(){clearTimeout(this.#timer);this.#timer=null;const update=this.#pending;this.#pending=null;if(update)this.publish(update)}
}
