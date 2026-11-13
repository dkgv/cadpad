import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {PrimitiveSpans,PickGeometry,scenePicking,pickTopology,pickTopologyInRect,geometryCenter,type PickInstance} from '../src/picking.ts';

const size={width:200,height:200};
test('indexed dense picking preserves bytes, native identities, clipping and transformed poses',()=>{
  const source=new THREE.SphereGeometry(4,32,20),wire=new THREE.WireframeGeometry(source);
  const buffers={vertices:source.attributes.position.array as Float32Array,triangles:Uint32Array.from(source.index!.array),triangles_per_face:new Uint32Array(source.index!.count/3).fill(1),edges:wire.attributes.position.array as Float32Array,segments_per_edge:new Uint32Array(wire.attributes.position.count/2).fill(1)};
  const bytes=Object.values(buffers).map(array=>Array.from(array));
  const pool=scenePicking(),geometry=pool(buffers),other=pool(buffers);
  const fastSurface=geometry.surface.raycast,fastLines=geometry.lines.raycast;
  assert.notEqual(fastSurface,THREE.Mesh.prototype.raycast);assert.notEqual(fastLines,THREE.LineSegments.prototype.raycast);
  const matrix=new THREE.Matrix4().compose(new THREE.Vector3(1,-1,0),new THREE.Quaternion().setFromEuler(new THREE.Euler(.2,.3,.4)),new THREE.Vector3(1.2,.8,1.1));
  const instances=[{instance_id:'dense',matrix,geometry}];
  for(const perspective of [false,true]){
    const view=camera(perspective);
    for(const near of [.1,19]){
      view.near=near;view.updateProjectionMatrix();
      for(const mode of ['face','edge','part'] as const)for(let i=0;i<30;i++){
        const point=new THREE.Vector2(Math.sin(i*2.399)*.7,Math.cos(i*1.71)*.7);
        geometry.surface.raycast=fastSurface;geometry.lines.raycast=fastLines;
        const indexed=pickTopology(view,point,size,mode,instances);
        geometry.surface.raycast=THREE.Mesh.prototype.raycast;geometry.lines.raycast=THREE.LineSegments.prototype.raycast;
        assert.deepEqual(indexed,pickTopology(view,point,size,mode,instances));
      }
    }
  }
  assert.deepEqual(Object.values(buffers).map(array=>Array.from(array)),bytes);
  assert.equal(geometry.surface.geometry,other.surface.geometry);
  geometry.dispose();assert.equal(pool.size,1);other.dispose();assert.equal(pool.size,0);
  source.dispose();wire.dispose();
});
function camera(perspective=false){
  const camera=perspective?new THREE.PerspectiveCamera(60,1,0.1,100):new THREE.OrthographicCamera(-10,10,10,-10,0.1,100);
  camera.position.set(0,0,20);camera.lookAt(0,0,0);camera.updateMatrixWorld();return camera;
}
function wire():PickInstance{
  return {instance_id:'wire',matrix:new THREE.Matrix4(),geometry:new PickGeometry({
    edges:new Float32Array([-5,0,0,0,0,0,0,0,0,5,0,0]),segments_per_edge:new Uint32Array([0,1,1]),
    vertices:new Float32Array(),triangles:new Uint32Array(),triangles_per_face:new Uint32Array()
  })};
}
function plate(z=0):PickInstance{
  return {instance_id:'plate',matrix:new THREE.Matrix4().makeTranslation(0,0,z),geometry:new PickGeometry({
    vertices:new Float32Array([-5,-5,0,5,-5,0,5,5,0,-5,5,0]),triangles:new Uint32Array([0,1,2,0,2,3]),triangles_per_face:new Uint32Array([1,0,1]),
    edges:new Float32Array(),segments_per_edge:new Uint32Array()
  })};
}
test('face and edge primitive spans preserve empty entries and reject invalid mappings',()=>{
  const spans=new PrimitiveSpans(new Uint32Array([2,0,3,0]),5);
  assert.deepEqual([-1,0,1,2,4,5,1.5].map(index=>spans.entity(index)),[null,0,0,2,2,null,null]);
  assert.deepEqual(spans.range(2),[2,5]);assert.equal(spans.range(4),null);
  assert.throws(()=>new PrimitiveSpans([1,2],4));assert.throws(()=>new PrimitiveSpans([-1],-1));
  assert.throws(()=>new PickGeometry({vertices:new Float32Array(3),triangles:new Uint32Array([0,1,0])}));
});
test('ray picks topology faces from immutable triangles and follows instance transforms',()=>{
  const instance=plate(),view=camera();
  assert.deepEqual(pickTopology(view,new THREE.Vector2(0.2,-0.2),size,'face',[instance]),{instance_id:'plate',entity_id:'face:0'});
  assert.deepEqual(pickTopology(view,new THREE.Vector2(-0.2,0.2),size,'face',[instance]),{instance_id:'plate',entity_id:'face:2'});
  instance.matrix.makeTranslation(7,0,0);
  assert.equal(pickTopology(view,new THREE.Vector2(-0.2,0.2),size,'face',[instance]),null);
  assert.deepEqual(pickTopology(view,new THREE.Vector2(0.7,0),size,'part',[instance]),{instance_id:'plate',entity_id:'part:0'});
  instance.geometry.dispose();
});
test('edge picking uses CSS pixels in orthographic and perspective views and supports wires',()=>{
  const instance=wire();
  for(const view of [camera(),camera(true)]){
    assert.deepEqual(pickTopology(view,new THREE.Vector2(-0.2,0.04),size,'edge',[instance]),{instance_id:'wire',entity_id:'edge:1'});
    assert.equal(pickTopology(view,new THREE.Vector2(-0.2,0.08),size,'edge',[instance]),null);
    assert.deepEqual(pickTopology(view,new THREE.Vector2(0.2,0),size,'part',[instance]),{instance_id:'wire',entity_id:'part:0'});
    assert.equal(pickTopology(view,new THREE.Vector2(),size,'face',[instance]),null);
  }
  instance.matrix.makeRotationZ(Math.PI/2).setPosition(3,0,0);
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(0.3,-0.2),size,'edge',[instance]),{instance_id:'wire',entity_id:'edge:1'});
  instance.geometry.dispose();
});
test('edge candidates behind visible faces are rejected and coplanar edges stay pickable',()=>{
  const edge=wire(),front=plate(1),view=camera();
  assert.equal(pickTopology(view,new THREE.Vector2(0.2,0),size,'edge',[edge,front]),null);
  front.matrix.identity();
  assert.deepEqual(pickTopology(view,new THREE.Vector2(0.2,0),size,'edge',[edge,front]),{instance_id:'wire',entity_id:'edge:2'});
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'edge',[]),null);
  front.matrix.makeTranslation(0,0,30);edge.matrix.makeTranslation(0,0,30);
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'edge',[edge,front]),null);
  edge.geometry.dispose();front.geometry.dispose();
});
test('face centers follow the indexed face instead of the shared part bounds',()=>{
  const geometry=new PickGeometry({
    vertices:new Float32Array([0,0,0,2,0,0,0,2,0,100,0,0,102,0,0,100,2,0]),
    triangles:new Uint32Array([0,1,2,3,4,5]),triangles_per_face:new Uint32Array([1,1]),
    edges:new Float32Array([-5,0,0,0,0,0,0,0,0,5,0,0]),segments_per_edge:new Uint32Array([1,1]),
  });
  const left=geometry.highlight('face',0)!,right=geometry.highlight('face',1)!;
  left.computeBoundingBox();
  assert.ok(left.boundingBox!.getCenter(new THREE.Vector3()).distanceTo(new THREE.Vector3(51,1,0))<1e-9);
  assert.ok(geometryCenter(left).distanceTo(new THREE.Vector3(1,1,0))<1e-9);
  assert.ok(geometryCenter(right).distanceTo(new THREE.Vector3(101,1,0))<1e-9);
  assert.ok(geometryCenter(geometry.highlight('edge',1)!).distanceTo(new THREE.Vector3(2.5,0,0))<1e-9);
  assert.ok(geometryCenter(geometry.highlight('part',0)!).distanceTo(new THREE.Vector3(51,1,0))<1e-9);
  geometry.dispose();
});
test('highlight geometry retains the selected topology subset without changing source arrays',()=>{
  const face=plate().geometry,edge=wire().geometry;
  const highlight=face.highlight('face',2)!;
  assert.deepEqual(Array.from(highlight.index!.array),[0,2,3]);
  assert.equal(highlight.attributes.position.array,face.buffers.vertices);
  assert.equal(face.highlight('face',1),null);assert.equal(face.highlight('face',3),null);
  const line=edge.highlight('edge',2)!;
  assert.deepEqual(Array.from(line.attributes.position.array),[0,0,0,5,0,0]);
  assert.equal(line.attributes.position.array.buffer,edge.buffers.edges.buffer);
  assert.equal(edge.highlight('part',0)!.attributes.position.count,4);
  highlight.dispose();line.dispose();face.dispose();edge.dispose();
});

test('camera near/far clipping excludes invisible topology from picks and occlusion',()=>{
  const edge=wire(),face=plate(),view=camera();
  view.far=10;view.updateProjectionMatrix();
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'face',[face]),null);
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'edge',[edge,face]),null);
  view.far=100;view.near=21;view.updateProjectionMatrix();
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'face',[face]),null);
  assert.equal(pickTopology(view,new THREE.Vector2(),size,'edge',[edge,face]),null);
  face.geometry.dispose();edge.geometry.dispose();
});

test('display topology maps sparse native identities and skips degenerate pole edges',()=>{
  const geometry=new PickGeometry({vertices:new Float32Array(),triangles:new Uint32Array(),triangles_per_face:new Uint32Array(),edges:new Float32Array([-5,-2,0,5,-2,0,-5,0,0,5,0,0,-5,2,0,5,2,0]),segments_per_edge:new Uint32Array([1,1,1])},{face_ids:[],edge_ids:[null,'edge:0','edge:3']});
  const instance={instance_id:'sphere',matrix:new THREE.Matrix4(),geometry};
  assert.equal(pickTopology(camera(),new THREE.Vector2(0,-0.2),size,'edge',[instance]),null);
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(0,0),size,'edge',[instance]),{instance_id:'sphere',entity_id:'edge:0'});
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(0,0.2),size,'edge',[instance]),{instance_id:'sphere',entity_id:'edge:3'});
  assert.deepEqual(Array.from(geometry.highlight('edge',0)!.attributes.position.array),[-5,0,0,5,0,0]);
  assert.equal(geometry.highlight('edge',1),null);geometry.dispose();
});

test('scene pooling shares immutable picking data while poses and ownership remain independent',()=>{
  const source=plate().geometry,pool=scenePicking();
  const a=pool(source.buffers),b=pool({...source.buffers});
  assert.equal(a.surface.geometry,b.surface.geometry);assert.equal(a.lines.geometry,b.lines.geometry);
  assert.notEqual(a.surface,b.surface);assert.notEqual(a.lines,b.lines);assert.equal(a.faces,b.faces);
  let released=0;a.surface.geometry.addEventListener('dispose',()=>released++);
  const first={instance_id:'first',matrix:new THREE.Matrix4(),geometry:a},second={instance_id:'second',matrix:new THREE.Matrix4().makeTranslation(8,0,0),geometry:b};
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(-0.2,0.2),size,'face',[first,second]),{instance_id:'first',entity_id:'face:2'});
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(0.8,0.2),size,'face',[first,second]),{instance_id:'second',entity_id:'face:2'});
  a.dispose();assert.equal(released,0);assert.equal(a.highlight('face',0),null);
  assert.ok(pickTopology(camera(),new THREE.Vector2(0.8,0.2),size,'face',[second]));
  b.dispose();assert.equal(released,1);assert.equal(pool.size,0);a.dispose();b.dispose();assert.equal(released,1);
  const next=pool(source.buffers);assert.notEqual(next.surface.geometry,a.surface.geometry);next.dispose();source.dispose();
});
test('pool keys include topology, byte view ranges and scene lifetime',()=>{
  const original=plate().geometry,pool=scenePicking(),a=pool(original.buffers);
  const mapped=pool(original.buffers,{face_ids:['face:7','face:8','face:9'],edge_ids:[]});
  assert.notEqual(mapped.surface.geometry,a.surface.geometry);
  const fresh=scenePicking()(original.buffers);assert.notEqual(fresh.surface.geometry,a.surface.geometry);
  const vertices=new Float32Array([0,0,0,1,0,0,0,1,0, 20,0,0,21,0,0,20,1,0]);
  const common={triangles:new Uint32Array([0,1,2]),triangles_per_face:new Uint32Array([1]),edges:new Float32Array(),segments_per_edge:new Uint32Array()};
  const left=pool({...common,vertices:vertices.subarray(0,9)}),right=pool({...common,vertices:vertices.subarray(9)});
  assert.notEqual(left.surface.geometry,right.surface.geometry);
  const duplicate=pool({...original.buffers,vertices:new Float32Array(original.buffers.vertices)});
  assert.notEqual(a.surface.geometry,duplicate.surface.geometry);
  for(const geometry of [a,mapped,fresh,left,right,duplicate,original])geometry.dispose();
});

test('reentrant disposal cannot acquire a resource while it is being released',()=>{
  const source=plate().geometry,pool=scenePicking(),old=pool(source.buffers);
  let replacement:PickGeometry|null=null;
  old.surface.geometry.addEventListener('dispose',()=>{replacement=pool(source.buffers);});
  old.dispose();assert.ok(replacement);
  const next=replacement as PickGeometry;assert.equal(pool.size,1);
  assert.notEqual(next.surface.geometry,old.surface.geometry);
  const overlay=next.highlight('face',0);assert.ok(overlay);overlay.dispose();next.dispose();assert.equal(pool.size,0);source.dispose();
});

test('overlapping revision swaps reuse picking geometry without retaining obsolete topology or instances',()=>{
  const source=plate().geometry,pool=scenePicking();let current=pool(source.buffers);
  const original=current.surface.geometry;
  for(let revision=0;revision<64;revision++){
    const views=Object.fromEntries(Object.entries(source.buffers).map(([name,array])=>[name,new (array.constructor as typeof Float32Array)(array.buffer,array.byteOffset,array.length)]));
    const next=pool(views);assert.equal(next.surface.geometry,original);assert.notEqual(next.surface,current.surface);
    current.dispose();assert.equal(pool.size,1);
    const instance={instance_id:`new-instance-${revision}`,matrix:new THREE.Matrix4().makeTranslation(5,0,0),geometry:next};
    assert.deepEqual(pickTopology(camera(),new THREE.Vector2(0.3,0.2),size,'face',[instance]),{instance_id:`new-instance-${revision}`,entity_id:'face:2'});
    current=next;
  }
  const remapped=pool(source.buffers,{face_ids:['face:10','face:11','face:12'],edge_ids:[]});
  assert.notEqual(remapped.surface.geometry,current.surface.geometry);assert.equal(pool.size,2);
  current.dispose();assert.equal(pool.size,1);
  assert.deepEqual(pickTopology(camera(),new THREE.Vector2(-0.2,0.2),size,'face',[{instance_id:'remapped',matrix:new THREE.Matrix4(),geometry:remapped}]),{instance_id:'remapped',entity_id:'face:12'});
  remapped.dispose();assert.equal(pool.size,0);source.dispose();
});

test('rectangle selection picks intersecting faces, parts and edges',()=>{
  const left=plate(),right={instance_id:'right',matrix:new THREE.Matrix4().makeTranslation(12,0,0),geometry:plate().geometry};
  const view=camera();
  // Centered rect covers the origin plate only.
  assert.deepEqual(
    pickTopologyInRect(view,{left:70,top:70,right:130,bottom:130},size,'part',[left,right]),
    [{instance_id:'plate',entity_id:'part:0'}],
  );
  // Wide rect covers both plates.
  assert.deepEqual(
    pickTopologyInRect(view,{left:40,top:70,right:190,bottom:130},size,'part',[left,right]).map(hit=>hit.instance_id).sort(),
    ['plate','right'],
  );
  const faces=pickTopologyInRect(view,{left:90,top:90,right:110,bottom:110},size,'face',[left]);
  assert.ok(faces.some(hit=>hit.entity_id==='face:0'||hit.entity_id==='face:2'));
  const edge=wire();
  assert.deepEqual(
    pickTopologyInRect(view,{left:40,top:95,right:160,bottom:105},size,'edge',[edge]),
    [{instance_id:'wire',entity_id:'edge:1'},{instance_id:'wire',entity_id:'edge:2'}],
  );
  assert.deepEqual(pickTopologyInRect(view,{left:0,top:0,right:10,bottom:10},size,'part',[left,right]),[]);
  left.geometry.dispose();right.geometry.dispose();edge.geometry.dispose();
});
