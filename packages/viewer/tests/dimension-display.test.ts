import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {dimensionStroke,moveDimensionStroke,placeDimensionLabels,angleGeometry,dimensionText,placeAnnotation} from '../src/dimension-display.ts';

const request=(x=100,y=100)=>({x,y,w:80,h:22,depth:0,offset:[0,0] as [number,number]});
test('crowded labels preserve input priority, displace with bounded search, then suppress',()=>{
  const boxes=placeDimensionLabels(Array.from({length:6},()=>request()),300,300);
  assert.deepEqual(boxes.map(b=>b?.y??null),[100,74,126,48,152,null]);
  for(const [i,a] of boxes.entries())for(const b of boxes.slice(i+1))if(a&&b){
    assert.ok(Math.abs(a.x-b.x)>=(a.w+b.w)/2+5||Math.abs(a.y-b.y)>=(a.h+b.h)/2+4);
  }
});
test('layout respects real measured height, preferred CSS offsets and canvas boundaries',()=>{
  const [box]=placeDimensionLabels([{...request(10,0),h:36,offset:[40,20]}],120,80);
  assert.deepEqual(box,{x:50,y:20,w:80,h:36});
  assert.deepEqual(placeDimensionLabels([request(10,60),request(140,60)],150,100),[null,null]);
  const [nearTop]=placeDimensionLabels([request(100,0)],300,300);
  assert.equal(nearTop?.y,26);
});
test('clipped, hidden, nonfinite or oversize labels cannot occupy space',()=>{
  const invalid=[{...request(),depth:1.01},{...request(),depth:-1.01},{...request(),depth:Infinity},{...request(),x:NaN},{...request(),w:400}];
  assert.deepEqual(placeDimensionLabels([...invalid,request()],300,300),[...invalid.map(()=>null),{x:100,y:100,w:80,h:22}]);
  assert.deepEqual(placeDimensionLabels([request()],0,0),[null]);
});
test('stroke geometry includes two inward arrowheads and extension quads with independent resources',()=>{
  const a=new THREE.Vector3(1,2,3),b=new THREE.Vector3(41,2,3);
  const mesh=dimensionStroke([{start:a,end:b,arrows:true},{start:a,end:new THREE.Vector3(1,-6,3)}]);
  const {position,peer,pixelOffset,endpoint}=mesh.geometry.attributes;
  assert.equal(position.count,18);
  for(const index of [6,9]){
    assert.equal(pixelOffset.getX(index),0);assert.equal(pixelOffset.getY(index),0);
    assert.equal(pixelOffset.getX(index+1),8);assert.equal(pixelOffset.getY(index+1),3);
    assert.equal(pixelOffset.getX(index+2),8);assert.equal(pixelOffset.getY(index+2),-3);
  }
  assert.equal(endpoint.getX(6),0);assert.equal(endpoint.getX(9),1);
  assert.deepEqual([position.getX(6),peer.getX(6)],[1,41]);
  assert.deepEqual([position.getX(9),peer.getX(9)],[41,1]);
  assert.equal(mesh.material.depthTest,false);assert.equal(mesh.material.depthWrite,false);assert.equal(mesh.frustumCulled,false);assert.equal(mesh.material.uniforms.width.value,1);
  const other=dimensionStroke([{start:a,end:b}]);assert.notEqual(other.geometry,mesh.geometry);assert.notEqual(other.material,mesh.material);
  mesh.geometry.dispose();mesh.material.dispose();other.geometry.dispose();other.material.dispose();
});
test('rigid motion reuses stroke attributes and updates all arrow and extension anchors',()=>{
  const start=new THREE.Vector3(),end=new THREE.Vector3(10,0,0);
  const mesh=dimensionStroke([{start,end,arrows:true}]),geometry=mesh.geometry,position=geometry.getAttribute('position');
  const movedStart=new THREE.Vector3(11,20,30),movedEnd=new THREE.Vector3(11,30,30);
  moveDimensionStroke(mesh,[{start:movedStart,end:movedEnd,arrows:true}]);
  assert.equal(mesh.geometry,geometry);assert.equal(geometry.getAttribute('position'),position);
  for(let i=0;i<position.count;i++){
    const isEnd=geometry.getAttribute('endpoint').getX(i);
    assert.deepEqual(new THREE.Vector3().fromBufferAttribute(position,i).toArray(),(isEnd?movedEnd:movedStart).toArray());
    assert.deepEqual(new THREE.Vector3().fromBufferAttribute(geometry.getAttribute('peer'),i).toArray(),(isEnd?movedStart:movedEnd).toArray());
  }
  mesh.geometry.dispose();mesh.material.dispose();
});
test('render callback requests CSS resolution rather than a device-pixel framebuffer size',()=>{
  const mesh=dimensionStroke([{start:new THREE.Vector3(),end:new THREE.Vector3(10,0,0)}]);
  const renderer={getSize:(target:THREE.Vector2)=>target.set(800,600),getDrawingBufferSize:()=>{throw new Error('device pixels must not size dimension strokes');}};
  mesh.onBeforeRender(renderer as never,null as never,null as never,null as never,null as never,null as never);
  assert.deepEqual(mesh.material.uniforms.resolution.value.toArray(),[800,600]);
  mesh.geometry.dispose();mesh.material.dispose();
});

test('angle arcs follow exact rays with inward end arrows, radial leaders and bisector labels',()=>{
  const vertex=new THREE.Vector3(5,6,7),start=vertex.clone().add(new THREE.Vector3(20,0,0)),end=vertex.clone().add(new THREE.Vector3(0,30,0));
  const arc=angleGeometry(start,vertex,end);
  assert.equal(arc.value,Math.PI/2);assert.equal(arc.segments.length,66);assert.equal(arc.valid,true);
  assert.ok(arc.segments[0].start.distanceTo(vertex.clone().add(new THREE.Vector3(6,0,0)))<1e-12);
  assert.ok(arc.segments[63].end.distanceTo(vertex.clone().add(new THREE.Vector3(0,6,0)))<1e-12);
  assert.equal(arc.segments[0].arrows,'start');assert.equal(arc.segments[63].arrows,'end');
  assert.deepEqual(arc.segments[64],{start:vertex,end:start});assert.deepEqual(arc.segments[65],{start:vertex,end});
  assert.ok(arc.anchor.distanceTo(vertex.clone().add(new THREE.Vector3(3*Math.SQRT2,3*Math.SQRT2,0)))<1e-12);
  const mesh=dimensionStroke(arc.segments);assert.equal(mesh.geometry.getAttribute('position').count,66*6+6);
  const moved=angleGeometry(start,vertex,vertex.clone().add(new THREE.Vector3(-20,0,0)));
  moveDimensionStroke(mesh,moved.segments);assert.equal(mesh.geometry.getAttribute('position').count,402);
  mesh.geometry.dispose();mesh.material.dispose();
});
test('angle presentation handles straight and coincident rays without NaNs and rejects collapsed rays',()=>{
  const origin=new THREE.Vector3(),a=new THREE.Vector3(10,0,0);
  for(const [end,value] of [[new THREE.Vector3(-10,0,0),Math.PI],[new THREE.Vector3(10,0,0),0]] as const){
    const arc=angleGeometry(a,origin,end);assert.equal(arc.value,value);assert.equal(arc.valid,true);
    assert.ok(arc.anchor.toArray().every(Number.isFinite));assert.equal(arc.segments.length,66);
  }
  const invalid=angleGeometry(origin,origin,a);assert.equal(invalid.valid,false);assert.equal(invalid.value,null);
  const quarter=angleGeometry(a,origin,new THREE.Vector3(0,10,0));
  const rotated=new THREE.Matrix4().makeRotationX(.7).setPosition(2,3,4);
  const moved=angleGeometry(a.clone().applyMatrix4(rotated),origin.clone().applyMatrix4(rotated),new THREE.Vector3(0,10,0).applyMatrix4(rotated));
  assert.ok(Math.abs(quarter.value!-moved.value!)<1e-12);
  assert.ok(quarter.anchor.clone().applyMatrix4(rotated).distanceTo(moved.anchor)<1e-12);
});
test('angle display converts radians explicitly and cannot label angular values with length units',()=>{
  assert.equal(dimensionText({label:'Included',kind:'angle',value:Math.PI/2}),'Included 90.00 deg');
  assert.equal(dimensionText({label:'Included',kind:'angle',value:Math.PI/2,format:{unit:'rad',precision:4}}),'Included 1.5708 rad');
  assert.equal(dimensionText({label:'Included',kind:'angle',value:Math.PI/2,format:{unit:'mm',precision:2}}),'Included · needs repair');
  assert.equal(dimensionText({label:'Length',kind:'distance',value:25.4,format:{unit:'in',precision:3}}),'Length 1.000 in');
  assert.equal(dimensionText({label:'Bore',kind:'diameter',value:6}),'Bore ⌀ 6.00 mm');
});

test('live angle placement transforms all three instances, preserves rigid values and suppresses collapsed poses',()=>{
  const annotation={kind:'angle',part:'left',vertex_part:'pivot',end_part:'right',start:[10,0,0] as [number,number,number],vertex:[0,0,0] as [number,number,number],end:[0,0,0] as [number,number,number],value:99};
  const transforms=new Map([['left',new THREE.Matrix4()],['pivot',new THREE.Matrix4()],['right',new THREE.Matrix4().makeTranslation(0,20,0)]]),cache=new WeakMap();
  assert.equal(placeAnnotation(annotation,transforms,cache)!.value,Math.PI/2);
  transforms.get('right')!.makeTranslation(20,20,0);
  assert.equal(placeAnnotation(annotation,transforms,cache)!.value,Math.PI/4);
  transforms.get('pivot')!.makeTranslation(5,0,0);
  assert.ok(Math.abs(placeAnnotation(annotation,transforms,cache)!.value!-Math.atan2(20,15))<1e-12);
  transforms.get('right')!.makeTranslation(5,0,0);
  const broken=placeAnnotation(annotation,transforms,cache)!;assert.equal(broken.valid,false);assert.equal(broken.value,undefined);
  const rigid={...annotation,vertex_part:'left',end_part:'left',end:[-10,0,0] as [number,number,number],value:Math.PI};
  const before=placeAnnotation(rigid,transforms,cache)!,local=cache.get(rigid);
  const matrix=new THREE.Matrix4().makeRotationZ(.8).setPosition(4,5,6);transforms.set('left',matrix);
  const moved=placeAnnotation(rigid,transforms,cache)!;
  assert.equal(moved.value,Math.PI);assert.equal(cache.get(rigid),local);
  assert.ok(moved.anchor.distanceTo(before.anchor.clone().applyMatrix4(matrix))<1e-12);
  transforms.delete('pivot');assert.equal(placeAnnotation(annotation,transforms,cache),null);
});

test('a moving angle retains its limiting plane at straight and folded poses',()=>{
  for(const side of [-1,1]){
    const annotation={kind:'angle',part:'left',vertex_part:'pivot',end_part:'right',start:[10,0,0] as [number,number,number],vertex:[0,0,0] as [number,number,number],end:[0,0,0] as [number,number,number],value:99};
    const transforms=new Map([['left',new THREE.Matrix4()],['pivot',new THREE.Matrix4()],['right',new THREE.Matrix4().makeTranslation(-10,0,side*1e-8)]]),cache=new WeakMap();
    const approaching=placeAnnotation(annotation,transforms,cache)!;
    assert.ok(Math.abs(approaching.value!-(Math.PI-1e-9))<1e-12);
    transforms.get('right')!.makeTranslation(-10,0,0);
    const straight=placeAnnotation(annotation,transforms,cache)!;
    assert.equal(straight.value,Math.PI);
    assert.ok(straight.anchor.distanceTo(approaching.anchor)<1e-8,'the arc must not jump into an unrelated plane at 180 degrees');
    // A temporarily collapsed ray must neither fabricate an angle nor erase
    // the last defined plane. A zero angle still has a valid numeric value.
    transforms.get('right')!.makeTranslation(0,0,0);
    assert.equal(placeAnnotation(annotation,transforms,cache)!.value,undefined);
    transforms.get('right')!.makeTranslation(10,0,0);
    assert.equal(placeAnnotation(annotation,transforms,cache)!.value,0);
    transforms.get('right')!.makeTranslation(-10,0,0);
    assert.ok(placeAnnotation(annotation,transforms,cache)!.anchor.distanceTo(straight.anchor)<1e-12);
    // Away from the singularity the unsigned interior angle must still choose
    // the plane of the current rays, including the opposite side after crossing.
    transforms.get('right')!.makeTranslation(-10,0,-side*.01);
    const crossed=placeAnnotation(annotation,transforms,cache)!;
    assert.ok(crossed.anchor.z*side<0);
    assert.ok(crossed.segments[63].end.distanceTo(new THREE.Vector3(-10,0,-side*.01).normalize().multiplyScalar(3))<1e-12);
  }
});

test('cross-part straight-angle layout follows the vertex frame under a common rigid motion',()=>{
  const annotation={kind:'angle',part:'left',vertex_part:'pivot',end_part:'right',start:[10,0,0] as [number,number,number],vertex:[0,0,0] as [number,number,number],end:[-10,0,0] as [number,number,number],value:99};
  const transforms=new Map([['left',new THREE.Matrix4()],['pivot',new THREE.Matrix4()],['right',new THREE.Matrix4()]]),cache=new WeakMap();
  const original=placeAnnotation(annotation,transforms,cache)!;
  const motion=new THREE.Matrix4().compose(new THREE.Vector3(20,-30,40),new THREE.Quaternion().setFromEuler(new THREE.Euler(.7,.4,.9)),new THREE.Vector3(1,1,1));
  for(const id of transforms.keys())transforms.set(id,motion.clone());
  for(const currentCache of [cache,new WeakMap()]){
    const moved=placeAnnotation(annotation,transforms,currentCache)!;
    assert.ok(Math.abs(moved.value!-Math.PI)<1e-12);
    assert.ok(moved.anchor.distanceTo(original.anchor.clone().applyMatrix4(motion))<1e-10);
    for(let i=0;i<66;i++){
      assert.ok(moved.segments[i].start.distanceTo(original.segments[i].start.clone().applyMatrix4(motion))<1e-10);
      assert.ok(moved.segments[i].end.distanceTo(original.segments[i].end.clone().applyMatrix4(motion))<1e-10);
    }
  }
});
