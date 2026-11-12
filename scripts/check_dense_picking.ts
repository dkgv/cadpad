/** CPU comparison against Three's original picker; does not measure GPU FPS. */
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {PickGeometry,pickTopology,type SelectionMode} from '../packages/viewer/src/picking.ts';
const source=new THREE.TorusGeometry(10,3,128,768),wire=new THREE.WireframeGeometry(source);
const buffers={vertices:source.attributes.position.array as Float32Array,triangles:Uint32Array.from(source.index!.array),triangles_per_face:new Uint32Array([source.index!.count/3]),edges:wire.attributes.position.array as Float32Array,segments_per_edge:new Uint32Array([wire.attributes.position.count/2])};
const start=performance.now(),geometry=new PickGeometry(buffers),prepare_ms=performance.now()-start;
const accelerated={surface:geometry.surface.raycast,lines:geometry.lines.raycast};
const instances=[{instance_id:'dense-model',matrix:new THREE.Matrix4(),geometry}];
const camera=new THREE.OrthographicCamera(-16,16,16,-16,.1,100);camera.position.set(0,0,40);camera.lookAt(0,0,0);camera.updateMatrixWorld();
const probes=Array.from({length:48},(_,i)=>new THREE.Vector2(Math.sin(i*2.399)*.9,Math.cos(i*1.71)*.9));
function run(mode:SelectionMode,fast:boolean){
 geometry.surface.raycast=fast?accelerated.surface:THREE.Mesh.prototype.raycast;
 geometry.lines.raycast=fast?accelerated.lines:THREE.LineSegments.prototype.raycast;
 const hits:unknown[]=[],times:number[]=[];
 for(const point of probes){const before=performance.now();hits.push(pickTopology(camera,point,{width:1000,height:1000},mode,instances));times.push(performance.now()-before);}
 times.sort((a,b)=>a-b);return {hits,mean_ms:times.reduce((a,b)=>a+b,0)/times.length,p95_ms:times[Math.ceil(times.length*.95)-1]};
}
const cases=[];
for(const mode of ['face','edge'] as const){
 run(mode,true);run(mode,false);
 const baseline=run(mode,false),indexed=run(mode,true);assert.deepEqual(indexed.hits,baseline.hits);
 cases.push({mode,baseline_mean_ms:baseline.mean_ms,indexed_mean_ms:indexed.mean_ms,baseline_p95_ms:baseline.p95_ms,indexed_p95_ms:indexed.p95_ms,speedup:baseline.mean_ms/indexed.mean_ms});
}
geometry.dispose();source.dispose();wire.dispose();
console.log(JSON.stringify({scope:'CPU picking only; no browser or GPU',triangles:buffers.triangles.length/3,edge_segments:buffers.edges.length/6,rays_per_case:probes.length,prepare_ms,cases},null,2));
