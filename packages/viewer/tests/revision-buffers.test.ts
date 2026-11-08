import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {revisionBuffers} from '../src/buffers.ts';
const bytes=(value:number)=>new Uint32Array([value]).buffer;
const descriptor=(data:ArrayBuffer,handle='buffer')=>({handle,bytes:data.byteLength,sha256:createHash('sha256').update(new Uint8Array(data)).digest('hex')});

test('verified content survives a revision and opaque-handle change within one workspace',async()=>{
  const cache=revisionBuffers(),data=bytes(7);let calls=0;
  const first=cache.begin('workspace',async()=>{calls++;return data;});
  assert.equal(await first.load(descriptor(data,'old-handle')),data);assert.equal(first.commit(),true);
  const second=cache.begin('workspace',async()=>{calls++;throw new Error('unchanged content must not transfer again');});
  const a=second.load(descriptor(data,'new-handle')),b=second.load(descriptor(data,'another-occurrence'));
  assert.equal(a,b);assert.equal(await a,data);assert.equal(calls,1);
  assert.deepEqual(second.stats,{fetchedBytes:0,reusedBytes:4});assert.equal(second.commit(),true);
  assert.deepEqual(cache.stats,{entries:1,bytes:4});cache.clear();assert.deepEqual(cache.stats,{entries:0,bytes:0});
});

test('workspace changes and content changes fetch fresh bytes',async()=>{
  const cache=revisionBuffers(),a=bytes(1),b=bytes(2);let calls=0;
  const first=cache.begin('one',async()=>{calls++;return a;});await first.load(descriptor(a));first.commit();
  const changed=cache.begin('one',async()=>{calls++;return b;});assert.equal(await changed.load(descriptor(b)),b);changed.commit();
  const fresh=b.slice(0),other=cache.begin('two',async()=>{calls++;return fresh;});
  assert.equal(await other.load(descriptor(b)),fresh);assert.notEqual(fresh,b);assert.equal(calls,3);other.commit();
  const unscoped=cache.begin(null,async()=>{calls++;return b.slice(0);});await unscoped.load(descriptor(b));unscoped.commit();
  assert.equal(calls,4);assert.equal(cache.stats.entries,0);
});

test('only current content within the byte budget is retained; oversized artifacts still load',async()=>{
  const cache=revisionBuffers(4),a=bytes(1),b=bytes(2),big=new Uint32Array([3,4]).buffer;
  const first=cache.begin('one',async handle=>handle==='a'?a:b);
  await first.load(descriptor(a,'a'));await first.load(descriptor(b,'b'));first.commit();
  assert.deepEqual(cache.stats,{entries:1,bytes:4});
  let calls=0;const next=cache.begin('one',async()=>{calls++;return b;});await next.load(descriptor(b));next.commit();assert.equal(calls,1);
  const oversized=cache.begin('one',async()=>big);assert.equal(await oversized.load(descriptor(big)),big);oversized.commit();
  assert.deepEqual(cache.stats,{entries:0,bytes:0});
  const empty=cache.begin('one',async()=>{throw new Error('no artifacts');});assert.equal(empty.commit(),true);
  assert.throws(()=>revisionBuffers(-1),/budget/);
});

test('failed and conflicting publications cannot replace committed cached content',async()=>{
  const cache=revisionBuffers(),a=bytes(1),b=bytes(2);
  const first=cache.begin('one',async()=>a);await first.load(descriptor(a));first.commit();
  const bad=cache.begin('one',async()=>b);await assert.rejects(bad.load(descriptor(bytes(3),'wrong-bytes')),/checksum/);
  assert.equal(bad.commit(),false);assert.deepEqual(cache.stats,{entries:1,bytes:4});
  const conflict=cache.begin('one',async()=>{throw new Error('valid A must be reused');});await conflict.load(descriptor(a,'same'));
  await assert.rejects(conflict.load(descriptor(b,'same')),/Conflicting/);assert.equal(conflict.commit(),false);
  const invalid=cache.begin('one',async()=>{throw new Error('invalid metadata must not fetch');});
  await assert.rejects(invalid.load({...descriptor(a),bytes:3}),/descriptor/);assert.equal(invalid.commit(),false);
  const recover=cache.begin('one',async()=>{throw new Error('committed A remains reusable');});assert.equal(await recover.load(descriptor(a)),a);assert.equal(recover.commit(),true);
});

test('mutated old arrays are detected and replaced rather than promoted to the next revision',async()=>{
  const cache=revisionBuffers(),original=bytes(3),expected=descriptor(original),first=cache.begin('one',async()=>original);
  await first.load(expected);first.commit();new Uint32Array(original)[0]=99;
  const fresh=bytes(3);let calls=0;const next=cache.begin('one',async()=>{calls++;return fresh;});
  assert.equal(await next.load(expected),fresh);assert.equal(calls,1);assert.deepEqual(next.stats,{fetchedBytes:4,reusedBytes:0});next.commit();
});

test('out-of-order, discarded and cleared loads cannot promote obsolete bytes',async()=>{
  const cache=revisionBuffers(),a=bytes(1),b=bytes(2);
  let finish!:(data:ArrayBuffer)=>void;
  const old=cache.begin('one',()=>new Promise(resolve=>{finish=resolve;}));const loading=old.load(descriptor(a));
  await Promise.resolve();
  const newer=cache.begin('one',async()=>b);await newer.load(descriptor(b));assert.equal(newer.commit(),true);
  finish(a);await loading;assert.equal(old.commit(),false);
  const retained=cache.begin('one',async()=>{throw new Error('B remains cached');});assert.equal(await retained.load(descriptor(b)),b);retained.commit();
  const discarded=cache.begin('one',async()=>a);await discarded.load(descriptor(a));discarded.discard();assert.equal(discarded.commit(),false);
  await assert.rejects(discarded.load(descriptor(a)),/closed/);
  const cleared=cache.begin('one',async()=>a);await cleared.load(descriptor(a));cache.clear();assert.equal(cleared.commit(),false);
  assert.deepEqual(cache.stats,{entries:0,bytes:0});
});

test('lack of secure hashing preserves loading while disabling cross-revision retention',async()=>{
  const previous=Object.getOwnPropertyDescriptor(globalThis,'crypto');
  Object.defineProperty(globalThis,'crypto',{configurable:true,value:undefined});
  try{
    const cache=revisionBuffers(),data=bytes(4);let calls=0;
    for(let index=0;index<2;index++){
      const plan=cache.begin('one',async()=>{calls++;return data.slice(0);});await plan.load(descriptor(data));plan.commit();
      assert.equal(cache.stats.entries,0);
    }
    assert.equal(calls,2);
  }finally{if(previous)Object.defineProperty(globalThis,'crypto',previous);else Reflect.deleteProperty(globalThis,'crypto');}
});
