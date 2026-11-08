import test from 'node:test';
import assert from 'node:assert/strict';
import {sceneBuffers} from '../src/buffers.ts';
const descriptor={handle:'a'.repeat(64)+'.bin',bytes:4,sha256:'a'.repeat(64)};
test('identical occurrences fetch one immutable buffer per scoped scene load',async()=>{
 let calls=0;const bytes=new ArrayBuffer(4),load=sceneBuffers(async()=>{calls++;await Promise.resolve();return bytes;});
 const buffers=await Promise.all(Array.from({length:100},()=>load(descriptor)));
 assert.equal(calls,1);assert.ok(buffers.every(buffer=>buffer===bytes));
 const other=sceneBuffers(async()=>{calls++;return new ArrayBuffer(4);});assert.notEqual(await other(descriptor),bytes);assert.equal(calls,2);
});
test('conflicting descriptors do not reuse cached bytes and invalid metadata does not fetch',async()=>{
 let calls=0;const load=sceneBuffers(async()=>{calls++;return new ArrayBuffer(4);});await load(descriptor);
 await assert.rejects(load({...descriptor,sha256:'b'.repeat(64)}),/Conflicting/);
 await assert.rejects(load({...descriptor,bytes:8}),/Conflicting/);
 await assert.rejects(load({...descriptor,sha256:'invalid'}),/descriptor/);assert.equal(calls,1);
});
test('mis-sized or unaligned transport buffers cannot enter the viewer',async()=>{
 await assert.rejects(sceneBuffers(async()=>new ArrayBuffer(8))(descriptor),/artifact/);
 await assert.rejects(sceneBuffers(async()=>new ArrayBuffer(3))({...descriptor,bytes:3}),/artifact/);
});
