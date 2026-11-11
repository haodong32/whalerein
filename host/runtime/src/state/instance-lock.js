// A separate SQLite transaction provides an OS-released instance lock without
// holding the operational state database's write lock or keeping stale PID files.
import { DatabaseSync } from 'node:sqlite'
import { join } from 'node:path'
import { mkdirSync } from 'node:fs'
import { PluginError } from '../errors.js'

export function lockInstance(directory) {
  mkdirSync(directory,{recursive:true,mode:0o700})
  const database=new DatabaseSync(join(directory,'runtime-lock.sqlite'))
  try {
    database.exec('PRAGMA busy_timeout=0; CREATE TABLE IF NOT EXISTS instance_guard (id INTEGER PRIMARY KEY); BEGIN EXCLUSIVE;')
  } catch(error) {
    database.close()
    throw new PluginError([5,6].includes(error.errcode)?'conflict':'internal','Host instance lock unavailable')
  }
  let closed=false
  return {close(){if(!closed){database.close();closed=true}}}
}
