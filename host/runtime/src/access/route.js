// One active Host route at a time.
//
// The Host persists a monotonic generation before it starts an attachment,
// retires the previous transport before the new node may dispatch, and rejects
// every asynchronous callback that belongs to a superseded generation. Central
// remains the authority for whether the route is open; this module only obeys
// the ordering rules and never invents authority.

import { ed25519 } from '@noble/curves/ed25519.js'

import { PluginError } from '../errors.js'

export const ROUTE_ATTACH_PREFIX = 'whalerein/v1/host-route|attach'

// The canonical bytes are shared with Access and Central. Field order and
// separators are a wire contract.
export function attachInput({ service, environment, hostId, nodeId, incarnation, challenge, generation, noiseKey, signedAtUnixMs }) {
  const noise = Buffer.from(noiseKey).toString('base64url')
  return `${ROUTE_ATTACH_PREFIX}|${service}|${environment}|${hostId}|${nodeId}|${incarnation}|${challenge}|${generation}|${noise}|${signedAtUnixMs}`
}

export function signRouteAttach(privateKey, tuple) {
  return ed25519.sign(new TextEncoder().encode(attachInput(tuple)), privateKey)
}

export class RouteJournal {
  #load
  #save
  #state

  constructor({ load, save }) {
    this.#load = load
    this.#save = save
    this.#state = { generation: 0, nodeId: '', incarnation: '', state: 'closed' }
  }

  async open() {
    const stored = await this.#load()
    if (stored && Number.isSafeInteger(stored.generation) && stored.generation >= 0) {
      this.#state = {
        generation: stored.generation,
        nodeId: String(stored.nodeId ?? ''),
        incarnation: String(stored.incarnation ?? ''),
        state: stored.state === 'open' ? 'open' : 'closed',
      }
    }
    return this.snapshot()
  }

  snapshot() {
    return { ...this.#state }
  }

  // Allocate and persist the next generation before any network operation.
  async allocate(nodeId, incarnation) {
    const next = this.#state.generation + 1
    this.#state = { generation: next, nodeId, incarnation, state: 'closed' }
    await this.#save(this.snapshot())
    return next
  }

  async commit({ generation, nodeId, incarnation, state }) {
    if (generation < this.#state.generation) {
      throw new RouteOrderError('stale generation')
    }
    this.#state = { generation, nodeId, incarnation, state }
    await this.#save(this.snapshot())
  }

  async floor(generation) {
    if (generation <= this.#state.generation) return this.#state.generation
    this.#state = { ...this.#state, generation, state: 'closed' }
    await this.#save(this.snapshot())
    return generation
  }
}

export class RouteOrderError extends PluginError {
  constructor(message) {
    super('route_stale', message)
  }
}

export class RouteCoordinator {
  #journal
  #retire
  #register
  #dispatch
  #candidates
  #backoffMs
  #minBackoffMs
  #maxBackoffMs
  #now
  #current

  constructor({ journal, retire, register, dispatch, backoffMs = 0, minBackoffMs = 250, maxBackoffMs = 30_000, now = () => Date.now() }) {
    this.#journal = journal
    this.#retire = retire
    this.#register = register
    this.#dispatch = dispatch
    this.#now = now
    this.#candidates = []
    this.#backoffMs = Math.max(0, backoffMs)
    this.#minBackoffMs = minBackoffMs
    this.#maxBackoffMs = maxBackoffMs
    this.#current = null
  }

  get current() {
    return this.#current ? { ...this.#current } : null
  }

  // Replace the eligible candidate list. A move never uses a list from another
  // workspace: the caller supplies only the authenticated directory answer.
  setCandidates(candidates) {
    this.#candidates = Array.isArray(candidates) ? candidates.slice() : []
  }

  // Attach to the first eligible candidate. Central may answer stale when it
  // already accepted a greater generation; adopt that floor and retry once.
  async attach(candidates = this.#candidates) {
    this.setCandidates(candidates)
    let lastReason = 'no_eligible_node'
    for (const candidate of this.#candidates) {
      const generation = await this.#journal.allocate(candidate.nodeId, candidate.incarnation)
      await this.#retire?.()
      this.#current = null
      const response = await this.#register({
        nodeId: candidate.nodeId,
        incarnation: candidate.incarnation,
        generation,
        challenge: candidate.challenge ?? '',
      })
      const outcome = response?.outcome
      if (outcome === 'committed' || outcome === 'idempotent') {
        const route = response.route ?? {}
        const committed = {
          generation: Number(route.generation ?? generation),
          nodeId: String(route.nodeId ?? candidate.nodeId),
          incarnation: String(route.incarnation ?? candidate.incarnation),
          state: 'open',
          httpsUrl: candidate.httpsUrl,
          mqttUrl: candidate.mqttUrl,
          observedAt: this.#now(),
        }
        await this.#journal.commit(committed)
        this.#current = committed
        this.#backoffMs = this.#minBackoffMs
        return committed
      }
      if (outcome === 'route_stale' || outcome === 'stale') {
        await this.#journal.floor(Number(response?.route?.generation ?? generation))
        lastReason = 'route_stale'
        continue
      }
      lastReason = String(response?.reason ?? outcome ?? 'route_refused')
      this.#current = null
      await this.#journal.commit({ generation, nodeId: candidate.nodeId, incarnation: candidate.incarnation, state: 'closed' })
    }
    throw new RouteOrderError(lastReason)
  }

  // A transport loss is the only signal that starts a move. A single client's
  // failure never calls this method.
  async transportLost(candidates = this.#candidates) {
    this.#current = null
    return this.attach(candidates)
  }

  // Run one asynchronous body only while it still belongs to the live
  // generation. Already-dispatched native work keeps its own command identity.
  async dispatch(generation, body) {
    if (!this.#current || this.#current.state !== 'open' || this.#current.generation !== generation) {
      throw new RouteOrderError('superseded connection generation')
    }
    return body()
  }
}