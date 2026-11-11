import test from 'node:test'
import assert from 'node:assert/strict'
import {mkdtempSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {lockInstance} from '../src/state/instance-lock.js'

test('one persistent Host owns an instance directory and releasing it permits a new runtime',()=>{
 const directory=mkdtempSync(join(tmpdir(),'whalerein-instance-'))
 try {
  const first=lockInstance(directory)
  assert.throws(()=>lockInstance(directory),e=>e.code==='conflict')
  first.close();first.close()
  const next=lockInstance(directory);next.close()
 }finally{rmSync(directory,{recursive:true,force:true})}
})
