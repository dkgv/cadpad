import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {WebGLAttributes} from 'three/src/renderers/webgl/WebGLAttributes.js';
import {WebGLGeometries} from 'three/src/renderers/webgl/WebGLGeometries.js';
import {shareRenderBuffers,revisionRenderBuffers} from '../src/render-buffers.ts';

/** Actual pinned Three upload/disposal code; only the WebGL device is a counter.
 * No browser, driver, pixels or GPU performance acceptance is implied. */
function renderer(){
  let id=0,uploads=0;const live=new Set<object>(),deleted=new Set<object>();
  const gl={ARRAY_BUFFER:34962,ELEMENT_ARRAY_BUFFER:34963,FLOAT:5126,UNSIGNED_INT:5125,
    createBuffer(){const buffer={id:++id};live.add(buffer);return buffer;},bindBuffer(){},bufferData(){uploads++;},bufferSubData(){},
    deleteBuffer(buffer:object){assert.ok(live.delete(buffer),'a live buffer is deleted exactly once');assert.ok(!deleted.has(buffer));deleted.add(buffer);}};
  const attributes=WebGLAttributes(gl),info={memory:{geometries:0}},bindings={releaseStatesOfGeometry(){}};
  const geometries=WebGLGeometries(gl,attributes,info,bindings);
  function draw(mesh:THREE.Mesh){
    const geometry=mesh.geometry;geometries.get(mesh,geometry);geometries.update(geometry);
    if(geometry.index)attributes.update(geometry.index,gl.ELEMENT_ARRAY_BUFFER);
    // Match the real renderer's upload-before-object-callback ordering.
    mesh.onBeforeRender(undefined as never,undefined as never,undefined as never,geometry,mesh.material as THREE.Material,null as never);
  }
  return {draw,attributes,live,info,get uploads(){return uploads;}};
}
function meshes(count=2){
  const position=new Float32Array([0,0,0,1,0,0,0,1,0]),normal=new Float32Array([0,0,1,0,0,1,0,0,1]),index=new Uint32Array([0,1,2]);
  return Array.from({length:count},(_,i)=>{
    const geometry=new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(position,3)).setAttribute('normal',new THREE.BufferAttribute(normal,3)).setIndex(new THREE.BufferAttribute(index,1));
    geometry.setAttribute('componentId',new THREE.Uint32BufferAttribute([i*3+1,i*3+2,i*3+3],1));
    const mesh=new THREE.Mesh(geometry,new THREE.MeshBasicMaterial({color:i?'red':'blue'}));mesh.position.x=i*10;return mesh;
  });
}
function replacement(parts:THREE.Mesh[]){
  return parts.map((part,index)=>{
    const geometry=new THREE.BufferGeometry();
    for(const [name,attribute]of Object.entries(part.geometry.attributes))if(name!=='componentId')geometry.setAttribute(name,new THREE.BufferAttribute(attribute.array,attribute.itemSize,attribute.normalized));
    if(part.geometry.index)geometry.setIndex(new THREE.BufferAttribute(part.geometry.index.array,1));
    geometry.setAttribute('componentId',new THREE.Uint32BufferAttribute([101+index,201+index,301+index],1));
    const result=new THREE.Mesh(geometry,new THREE.MeshBasicMaterial({color:'green'}));result.position.x=50+index;return result;
  });
}

test('render buffers share uploads while keeping geometry, IDs, materials and transforms independent',()=>{
  const [a,b]=meshes(),pool=shareRenderBuffers([a,b]),gpu=renderer();
  assert.notEqual(a.geometry,b.geometry);
  assert.equal(a.geometry.attributes.position,b.geometry.attributes.position);
  assert.equal(a.geometry.attributes.normal,b.geometry.attributes.normal);assert.equal(a.geometry.index,b.geometry.index);
  assert.notEqual(a.geometry.attributes.componentId,b.geometry.attributes.componentId);
  assert.deepEqual([...b.geometry.attributes.componentId.array],[4,5,6]);
  assert.notEqual(a.material,b.material);assert.equal(a.position.x,0);assert.equal(b.position.x,10);
  gpu.draw(a);gpu.draw(b);assert.equal(gpu.uploads,5);assert.equal(gpu.live.size,5);
  const position=gpu.attributes.get(a.geometry.attributes.position).buffer;
  a.geometry.dispose();assert.equal(gpu.live.size,4);assert.ok(gpu.live.has(position));assert.equal(gpu.info.memory.geometries,1);
  a.geometry.dispose();assert.ok(gpu.live.has(position));
  gpu.draw(a);assert.equal(gpu.live.size,5);assert.equal(gpu.uploads,6);assert.equal(gpu.attributes.get(a.geometry.attributes.position).buffer,position);
  pool.dispose();assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);pool.dispose();
});

test('interleaved edge start/end views share one upload and keep instance IDs separate',()=>{
  const array=new Float32Array([0,0,0,1,0,0,1,0,0,1,1,0]);
  const parts=Array.from({length:2},(_,i)=>{
    const geometry=new THREE.InstancedBufferGeometry(),data=new THREE.InstancedInterleavedBuffer(array,6,1);
    geometry.setAttribute('instanceStart',new THREE.InterleavedBufferAttribute(data,3,0));
    geometry.setAttribute('instanceEnd',new THREE.InterleavedBufferAttribute(data,3,3));
    geometry.setAttribute('componentId',new THREE.InstancedBufferAttribute(new Uint32Array([i+1,i+1]),1));
    return new THREE.Mesh(geometry,new THREE.MeshBasicMaterial());
  });
  const pool=shareRenderBuffers(parts),gpu=renderer();gpu.draw(parts[0]);gpu.draw(parts[1]);
  assert.equal(parts[0].geometry.attributes.instanceStart,parts[1].geometry.attributes.instanceStart);
  assert.equal((parts[1].geometry.attributes.instanceStart as THREE.InterleavedBufferAttribute).data,(parts[1].geometry.attributes.instanceEnd as THREE.InterleavedBufferAttribute).data);
  assert.equal(gpu.uploads,3);parts[0].geometry.dispose();assert.equal(gpu.live.size,2);pool.dispose();assert.equal(gpu.live.size,0);
});

test('all owners can dispose and later reactivate without retaining old device buffers',()=>{
  const parts=meshes(),pool=shareRenderBuffers(parts),gpu=renderer();for(const part of parts)gpu.draw(part);
  for(const part of parts)part.geometry.dispose();assert.equal(gpu.live.size,0);
  gpu.draw(parts[0]);assert.equal(gpu.live.size,4);gpu.draw(parts[1]);assert.equal(gpu.live.size,5);
  pool.dispose();assert.equal(gpu.live.size,0);
});

test('dynamic attributes, different byte views and separate scenes cannot share accidentally',()=>{
  const [a,b]=meshes();a.geometry.attributes.position.setUsage(THREE.DynamicDrawUsage);
  const buffer=new ArrayBuffer(72);a.geometry.setAttribute('normal',new THREE.BufferAttribute(new Float32Array(buffer,0,9),3));b.geometry.setAttribute('normal',new THREE.BufferAttribute(new Float32Array(buffer,36,9),3));
  const first=shareRenderBuffers([a,b]);assert.notEqual(a.geometry.attributes.position,b.geometry.attributes.position);assert.notEqual(a.geometry.attributes.normal,b.geometry.attributes.normal);
  const [c,d]=meshes(),second=shareRenderBuffers([c,d]);assert.notEqual(c.geometry.index,a.geometry.index);
  first.dispose();second.dispose();
});

test('original render and shadow callbacks retain their receiver and are restored at teardown',()=>{
  const [a,b]=meshes();let renders=0,shadows=0;
  const beforeRender=function(this:THREE.Object3D){assert.equal(this,a);renders++;};
  const beforeShadow=function(this:THREE.Object3D){assert.equal(this,a);shadows++;};
  a.onBeforeRender=beforeRender;a.onBeforeShadow=beforeShadow;
  const pool=shareRenderBuffers([a,b]),gpu=renderer();gpu.draw(a);gpu.draw(b);
  a.geometry.dispose();b.geometry.dispose();assert.equal(gpu.live.size,0);
  // A shadow pass can reactivate a disposed geometry before a normal pass.
  a.onBeforeShadow(undefined as never,undefined as never,undefined as never,undefined as never,a.geometry,a.material,null as never);
  gpu.draw(a);assert.equal(renders,2);assert.equal(shadows,1);
  pool.dispose();assert.equal(gpu.live.size,0);assert.equal(a.onBeforeRender,beforeRender);assert.equal(a.onBeforeShadow,beforeShadow);
});

test('never-uploaded owners do not retain device buffers regardless of teardown order',()=>{
  for(const uploaded of [0,1]){
    const parts=meshes(),pool=shareRenderBuffers(parts),gpu=renderer();gpu.draw(parts[uploaded]);
    assert.equal(gpu.live.size,4);pool.dispose();assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);
  }
});

test('GPU registration without a visible render callback still owns the shared buffers',()=>{
  const [a,b]=meshes(),pool=shareRenderBuffers([a,b]),gpu=renderer();
  // A material-hidden object can upload during projection but skip its draw.
  const before=b.onBeforeRender;b.onBeforeRender=()=>{};gpu.draw(a);gpu.draw(b);b.onBeforeRender=before;
  a.geometry.dispose();assert.equal(gpu.live.size,4);pool.dispose();assert.equal(gpu.live.size,0);
});

test('context restoration forgets old upload leases when only some objects return',()=>{
  const [a,b]=meshes(),events=new EventTarget(),pool=shareRenderBuffers([a,b],events),old=renderer();old.draw(a);old.draw(b);
  events.dispatchEvent(new Event('webglcontextlost'));
  const restored=renderer();restored.draw(a);pool.dispose();assert.equal(restored.live.size,0);assert.equal(old.live.size,0);
});

test('geometry without an occurrence-private upload marker remains unpooled',()=>{
  const [a,b]=meshes();a.geometry.deleteAttribute('componentId');b.geometry.deleteAttribute('componentId');
  const before=a.geometry.dispose,pool=shareRenderBuffers([a,b]);
  assert.equal(pool.stats.geometries,0);assert.notEqual(a.geometry.attributes.position,b.geometry.attributes.position);assert.equal(a.geometry.dispose,before);pool.dispose();
});

test('half-float and integer interpretations of the same underlying bytes remain distinct',()=>{
  const [a,b]=meshes(),bits=new Uint16Array([0,0,0,15360,0,0,0,15360,0]);
  const half=new THREE.Float16BufferAttribute(bits,3),integer=new THREE.BufferAttribute(bits,3);half.array=bits;
  a.geometry.setAttribute('position',half);b.geometry.setAttribute('position',integer);
  const pool=shareRenderBuffers([a,b]);assert.notEqual(a.geometry.attributes.position,b.geometry.attributes.position);pool.dispose();
});

test('revision handoff reuses uploaded static buffers and keeps new picking IDs and transforms private',()=>{
  const cache=revisionRenderBuffers(),gpu=renderer(),old=meshes(),scene=cache.attach(old);
  for(const part of old)gpu.draw(part);
  const position=gpu.attributes.get(old[0].geometry.attributes.position).buffer;
  const next=replacement(old);
  cache.beginSwap();scene.dispose();for(const part of old)part.geometry.dispose();
  const incoming=cache.attach(next);cache.finishSwap();
  assert.equal(cache.stats.retainedGeometries,1);
  for(const part of next)gpu.draw(part);
  assert.equal(gpu.uploads,7,'only two private ID buffers upload after the original five buffers');
  assert.equal(gpu.attributes.get(next[0].geometry.attributes.position).buffer,position);
  assert.notEqual(next[0].geometry.attributes.componentId,old[0].geometry.attributes.componentId);
  assert.deepEqual([...next[0].geometry.attributes.componentId.array],[101,201,301]);assert.equal(next[0].position.x,50);
  assert.equal(cache.stats.retainedGeometries,0);incoming.dispose();cache.dispose();assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);
});

test('never-uploaded incoming scenes still release the registered old owner and every buffer',()=>{
  for(const budget of [0,64*1024*1024]){
    const cache=revisionRenderBuffers(undefined,budget),gpu=renderer(),old=meshes(),scene=cache.attach(old);for(const part of old)gpu.draw(part);
    cache.beginSwap();scene.dispose();const incoming=cache.attach(replacement(old));cache.finishSwap();
    assert.ok(cache.stats.retainedBytes<=budget);incoming.dispose();cache.dispose();
    assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);
  }
  const cache=revisionRenderBuffers(),gpu=renderer(),old=meshes(),scene=cache.attach(old);gpu.draw(old[0]);
  cache.beginSwap();scene.dispose();cache.finishSwap();assert.equal(gpu.live.size,0);cache.dispose();
});

test('changed attributes re-upload while unchanged normals and indices retain the same device buffers',()=>{
  const cache=revisionRenderBuffers(),gpu=renderer(),old=meshes(1),scene=cache.attach(old);gpu.draw(old[0]);
  const normal=gpu.attributes.get(old[0].geometry.attributes.normal).buffer,position=gpu.attributes.get(old[0].geometry.attributes.position).buffer;
  const next=replacement(old);next[0].geometry.setAttribute('position',new THREE.Float32BufferAttribute([0,0,0,2,0,0,0,2,0],3));
  cache.beginSwap();scene.dispose();const incoming=cache.attach(next);cache.finishSwap();gpu.draw(next[0]);
  assert.equal(gpu.uploads,6);assert.ok(!gpu.live.has(position));assert.equal(gpu.attributes.get(next[0].geometry.attributes.normal).buffer,normal);
  incoming.dispose();cache.dispose();assert.equal(gpu.live.size,0);
});

test('context loss retires dormant keepers and restored uploads start without phantom ownership',()=>{
  for(const duringSwap of [false,true]){
    const events=new EventTarget(),cache=revisionRenderBuffers(events),oldDevice=renderer(),old=meshes(1),scene=cache.attach(old);oldDevice.draw(old[0]);
    cache.beginSwap();scene.dispose();const next=replacement(old);
    if(duringSwap)events.dispatchEvent(new Event('webglcontextlost'));
    const incoming=cache.attach(next);cache.finishSwap();
    if(!duringSwap)events.dispatchEvent(new Event('webglcontextlost'));
    assert.equal(cache.stats.retainedGeometries,0);assert.equal(oldDevice.live.size,0);
    const restored=renderer();restored.draw(next[0]);assert.equal(restored.uploads,4);
    incoming.dispose();cache.dispose();assert.equal(restored.live.size,0);assert.equal(restored.info.memory.geometries,0);
  }
});

test('repeated drawn and dormant swaps keep retained owners bounded and fully retire at teardown',()=>{
  const cache=revisionRenderBuffers(undefined,1024),gpu=renderer();let parts=meshes(),scene=cache.attach(parts);for(const part of parts)gpu.draw(part);
  for(let revision=0;revision<64;revision++){
    const next=replacement(parts);cache.beginSwap();scene.dispose();scene=cache.attach(next);cache.finishSwap();
    if(revision%3===0)for(const part of next)gpu.draw(part);
    assert.ok(cache.stats.retainedBytes<=1024);assert.ok(cache.stats.retainedGeometries<=1);assert.ok(gpu.live.size<=5);parts=next;
  }
  scene.dispose();cache.dispose();cache.dispose();assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);
});

test('modified canonical attributes cannot be adopted as static revision storage',()=>{
  const cache=revisionRenderBuffers(),gpu=renderer(),old=meshes(1),scene=cache.attach(old);gpu.draw(old[0]);
  old[0].geometry.attributes.position.needsUpdate=true;
  const next=replacement(old);cache.beginSwap();scene.dispose();const incoming=cache.attach(next);cache.finishSwap();
  assert.notEqual(next[0].geometry.attributes.position,old[0].geometry.attributes.position);
  gpu.draw(next[0]);assert.equal(gpu.uploads,6);incoming.dispose();cache.dispose();assert.equal(gpu.live.size,0);
});

test('interleaved revision handoff retains one line data buffer with independent occurrence IDs',()=>{
  const data=new Float32Array([0,0,0,1,0,0]);
  function edge(id:number){const geometry=new THREE.InstancedBufferGeometry(),storage=new THREE.InstancedInterleavedBuffer(data,6,1);
    geometry.setAttribute('instanceStart',new THREE.InterleavedBufferAttribute(storage,3,0));geometry.setAttribute('instanceEnd',new THREE.InterleavedBufferAttribute(storage,3,3));
    geometry.setAttribute('componentId',new THREE.InstancedBufferAttribute(new Uint32Array([id]),1));return new THREE.Mesh(geometry,new THREE.MeshBasicMaterial());}
  const cache=revisionRenderBuffers(),gpu=renderer(),a=edge(1),old=cache.attach([a]);gpu.draw(a);
  const buffer=gpu.attributes.get((a.geometry.attributes.instanceStart as THREE.InterleavedBufferAttribute).data).buffer;
  cache.beginSwap();old.dispose();const b=edge(2),next=cache.attach([b]);cache.finishSwap();gpu.draw(b);
  assert.equal(gpu.uploads,3);assert.equal(gpu.attributes.get((b.geometry.attributes.instanceStart as THREE.InterleavedBufferAttribute).data).buffer,buffer);
  assert.equal((b.geometry.attributes.instanceStart as THREE.InterleavedBufferAttribute).data,(b.geometry.attributes.instanceEnd as THREE.InterleavedBufferAttribute).data);
  next.dispose();cache.dispose();assert.equal(gpu.live.size,0);
});

test('partial attachment rolls back callbacks and registered keepers without taking another scene ownership',()=>{
  const cache=revisionRenderBuffers(),gpu=renderer(),old=meshes(1),scene=cache.attach(old);gpu.draw(old[0]);
  assert.throws(()=>cache.attach(old),/already owned/);assert.equal(gpu.live.size,4);
  const next=replacement(old),original=next[0].geometry.dispose,upload=next[0].geometry.attributes.componentId.onUploadCallback;
  const root=new THREE.Group();root.add(next[0]);const traverse=root.traverse;let pass=0;
  root.traverse=function(callback){pass++;if(pass===2){callback(this);callback(next[0]);throw new Error('interrupted attachment');}traverse.call(this,callback);};
  cache.beginSwap();scene.dispose();assert.throws(()=>cache.attach([root]),/interrupted attachment/);
  assert.equal(next[0].geometry.dispose,original);assert.equal(next[0].geometry.attributes.componentId.onUploadCallback,upload);
  assert.equal(cache.stats.geometries,0);assert.equal(gpu.live.size,0);assert.equal(gpu.info.memory.geometries,0);cache.dispose();
});
