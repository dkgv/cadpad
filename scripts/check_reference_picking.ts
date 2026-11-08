/** CPU geometry/picking only. No browser, renderer, WebGL context or UI is created. */
import assert from 'node:assert/strict';
import {mkdtemp,readFile,rm} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {tmpdir,cpus,platform,arch,release} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import * as THREE from 'three';
import {sceneBuffers,type BufferDescriptor} from '../packages/viewer/src/buffers.ts';
import {scenePicking,pickTopology,type MeshBuffers,type PickInstance,type SelectionMode} from '../packages/viewer/src/picking.ts';

interface Part {instance_id:string;position:[number,number,number];mesh:{buffers:Record<string,BufferDescriptor>;face_ids:(string|null)[];edge_ids:(string|null)[]}}
function stats(values:number[]){const sorted=[...values].sort((a,b)=>a-b);return {samples:values.length,mean_ms:values.reduce((a,b)=>a+b,0)/values.length,p95_ms:sorted[Math.ceil(sorted.length*0.95)-1],max_ms:sorted.at(-1)};}
async function measure(side:number){
 const temp=await mkdtemp(join(tmpdir(),'cadpad-picking-'));
 try{
  const repository=fileURLToPath(new URL('../',import.meta.url));const directory=join(temp,'scene');
  const prepared=spawnSync(join(repository,'.venv/bin/python'),[join(repository,'scripts/check_reference_scene.py'),'--grid-side',String(side),'--artifact-directory',directory],{encoding:'utf8',timeout:180000,maxBuffer:4*1024*1024});
  assert.equal(prepared.status,0,prepared.stderr);const geometryReport=JSON.parse(prepared.stdout);
  const model=JSON.parse(await readFile(join(directory,'scene.json'),'utf8')) as {parts:Part[]};
  const descriptors=new Map<string,BufferDescriptor>();for(const part of model.parts)for(const info of Object.values(part.mesh.buffers))descriptors.set(info.handle,info);
  let fetches=0,bytes=0;const loader=sceneBuffers(async handle=>{fetches++;const data=await readFile(join(directory,handle));bytes+=data.byteLength;assert.equal(createHash('sha256').update(data).digest('hex'),descriptors.get(handle)!.sha256);return Uint8Array.from(data).buffer;});
  const prepare=scenePicking();const start=performance.now();const instances:PickInstance[]=[];
  for(const part of model.parts){
   const buffers:MeshBuffers={};for(const [key,info] of Object.entries(part.mesh.buffers)){const data=await loader(info);buffers[key]=['triangles','triangles_per_face','segments_per_edge','face_types','edge_types'].includes(key)?new Uint32Array(data):new Float32Array(data);}
   instances.push({instance_id:part.instance_id,matrix:new THREE.Matrix4().makeTranslation(...part.position),geometry:prepare(buffers,part.mesh)});
  }
  const preparationMs=performance.now()-start;
  assert.equal(fetches,descriptors.size);assert.equal(bytes,geometryReport.unique_buffer_bytes);
  const height=(side*240)*1.2,width=height*1.6,camera=new THREE.OrthographicCamera(-width/2,width/2,height/2,-height/2,0.1,20000);camera.position.set(0,0,10000);camera.lookAt(0,0,0);camera.updateMatrixWorld();
  const size={width:1600,height:1000};
  function probe(mode:SelectionMode,index:number){
   const part=model.parts[index];const point=new THREE.Vector3(...part.position);if(mode==='edge')point.x+=60;
   const ndc=point.project(camera);const start=performance.now();const hit=pickTopology(camera,new THREE.Vector2(ndc.x,ndc.y),size,mode,instances);const elapsed=performance.now()-start;
   assert.deepEqual(hit,{instance_id:part.instance_id,entity_id:mode==='edge'?'edge:0':'face:0'});return elapsed;
  }
  const coldFaceMs=probe('face',0);
  for(let i=0;i<32;i++){probe('face',(i*37)%instances.length);probe('edge',(i*37)%instances.length);}
  const face:number[]=[],edge:number[]=[],miss:number[]=[];
  for(let i=0;i<160;i++){
   face.push(probe('face',(i*37)%instances.length));edge.push(probe('edge',(i*37)%instances.length));
   const start=performance.now();assert.equal(pickTopology(camera,new THREE.Vector2(0,0),size,'face',instances),null);miss.push(performance.now()-start);
  }
  const uniqueSurfaceGeometries=new Set(instances.map(instance=>instance.geometry.surface.geometry)).size;
  const uniqueEdgeGeometries=new Set(instances.map(instance=>instance.geometry.lines.geometry)).size;
  assert.equal(uniqueSurfaceGeometries,1);assert.equal(uniqueEdgeGeometries,1);
  for(const instance of instances)instance.geometry.dispose();
  return {geometry:geometryReport,preparation_ms:preparationMs,first_face_pick_ms:coldFaceMs,face:stats(face),edge:stats(edge),empty_ray:stats(miss),artifact_fetches:fetches,artifact_bytes:bytes,unique_cpu_surface_geometries:uniqueSurfaceGeometries,unique_cpu_edge_geometries:uniqueEdgeGeometries,viewport_css_pixels:size,camera:'orthographic top',warmup_pairs:32};
 }finally{await rm(temp,{recursive:true,force:true});}
}
const repository=fileURLToPath(new URL('../',import.meta.url));
const fingerprints:Record<string,string>={};for(const name of ['packages/viewer/src/picking.ts','packages/viewer/src/buffers.ts','scripts/check_reference_picking.ts','package-lock.json'])fingerprints[name]=createHash('sha256').update(await readFile(join(repository,name))).digest('hex');
const cases=[];for(const side of [10,20])cases.push(await measure(side));
process.stdout.write(JSON.stringify({status:'passed',checked_at:new Date().toISOString(),platform:`${platform()} ${release()} ${arch()}`,cpu:cpus()[0]?.model,node:process.version,v8:process.versions.v8,three_revision:THREE.REVISION,fingerprints,scope:'actual adapter CPU picking and buffer load with file-backed trusted fixtures; excludes native desktop transport, DOM, GPU and visible response',cases},null,2)+'\n');
