/** Real adapter byte/picking ownership with file-backed reference artifacts.
 * No browser, native desktop IPC, renderer or physical memory measurement. */
import assert from 'node:assert/strict';
import {mkdtemp,readFile,rm} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {tmpdir,cpus,platform,arch,release} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import * as THREE from 'three';
import {sceneBuffers,revisionBuffers,type BufferDescriptor} from '../packages/viewer/src/buffers.ts';
import {scenePicking,pickTopology,type MeshBuffers,type PickInstance} from '../packages/viewer/src/picking.ts';

interface Part {instance_id:string;position:[number,number,number];mesh:{buffers:Record<string,BufferDescriptor>;face_ids:(string|null)[];edge_ids:(string|null)[]}}
const repository=fileURLToPath(new URL('../',import.meta.url));
async function measure(side:number){
  const temp=await mkdtemp(join(tmpdir(),'cadpad-revision-bytes-'));
  try{
    const directory=join(temp,'scene');
    const prepared=spawnSync(join(repository,'.venv/bin/python'),[join(repository,'scripts/check_reference_scene.py'),'--grid-side',String(side),'--artifact-directory',directory],{encoding:'utf8',timeout:180000,maxBuffer:4*1024*1024});
    assert.equal(prepared.status,0,prepared.stderr);
    const geometry=JSON.parse(prepared.stdout),model=JSON.parse(await readFile(join(directory,'scene.json'),'utf8')) as {parts:Part[]};
    const descriptors=new Map(model.parts.flatMap(part=>Object.values(part.mesh.buffers).map(info=>[info.handle,info] as const)));
    const height=side*240*1.2,width=height*1.6,camera=new THREE.OrthographicCamera(-width/2,width/2,height/2,-height/2,.1,20000);
    camera.position.set(0,0,10000);camera.lookAt(0,0,0);camera.updateMatrixWorld();
    async function trial(reuse:boolean){
      const cache=revisionBuffers();let pool=scenePicking(),current:PickInstance[]=[],fetches=0,transferred=0;
      const records=[];let firstGeometry:THREE.BufferGeometry|null=null;
      for(let revision=0;revision<=32;revision++){
        const beforeFetches=fetches,beforeBytes=transferred;
        const artifact=async(handle:string)=>{
          fetches++;const bytes=await readFile(join(directory,handle));transferred+=bytes.byteLength;
          assert.equal(createHash('sha256').update(bytes).digest('hex'),descriptors.get(handle)!.sha256);
          return Uint8Array.from(bytes).buffer;
        };
        const plan=reuse?cache.begin('reference-workspace',artifact):null;
        const load=plan?(info:BufferDescriptor)=>plan.load(info):sceneBuffers(artifact);
        if(!reuse)pool=scenePicking();
        const start=performance.now(),next:PickInstance[]=[];
        for(const [index,part]of model.parts.entries()){
          const buffers:MeshBuffers={};
          for(const [name,info]of Object.entries(part.mesh.buffers)){
            const data=await load(info);buffers[name]=['triangles','triangles_per_face','segments_per_edge','face_types','edge_types'].includes(name)?new Uint32Array(data):new Float32Array(data);
          }
          next.push({instance_id:`revision-${revision}-${index}`,matrix:new THREE.Matrix4().makeTranslation(part.position[0]+revision,part.position[1],part.position[2]),geometry:pool(buffers,part.mesh)});
        }
        const elapsed=performance.now()-start;
        if(!firstGeometry)firstGeometry=next[0].geometry.surface.geometry;
        else if(reuse)assert.equal(next[0].geometry.surface.geometry,firstGeometry);
        else assert.notEqual(next[0].geometry.surface.geometry,firstGeometry);
        for(const instance of current)instance.geometry.dispose();current=next;
        assert.equal(pool.size,1);
        const index=revision*37%model.parts.length,part=model.parts[index],point=new THREE.Vector3(part.position[0]+revision,part.position[1],part.position[2]).project(camera);
        assert.deepEqual(pickTopology(camera,new THREE.Vector2(point.x,point.y),{width:1600,height:1000},'face',current),{instance_id:`revision-${revision}-${index}`,entity_id:'face:0'});
        if(plan)assert.equal(plan.commit(),true);
        records.push({revision,preparation_ms:elapsed,artifact_fetches:fetches-beforeFetches,artifact_bytes:transferred-beforeBytes,cache:cache.stats});
      }
      if(reuse){assert.equal(fetches,records[0].artifact_fetches);assert.equal(transferred,geometry.unique_buffer_bytes);assert.ok(records.slice(1).every(record=>record.artifact_bytes===0));}
      for(const instance of current)instance.geometry.dispose();assert.equal(pool.size,0);cache.clear();assert.equal(cache.stats.bytes,0);
      return {artifact_fetches:fetches,artifact_bytes:transferred,revisions:records,retired_cpu_resources:pool.size,retired_cache_bytes:cache.stats.bytes};
    }
    const baseline=await trial(false),reused=await trial(true);
    return {instances:model.parts.length,input_geometry:geometry,baseline,reused,saved_artifact_bytes:baseline.artifact_bytes-reused.artifact_bytes};
  }finally{await rm(temp,{recursive:true,force:true});}
}
const cases=[];for(const side of [10,20])cases.push(await measure(side));
const fingerprints:Record<string,string>={};
for(const name of ['packages/viewer/src/buffers.ts','packages/viewer/src/picking.ts','packages/viewer/src/index.ts','apps/desktop/src/app.tsx','scripts/check_revision_buffers.ts','package-lock.json'])fingerprints[name]=createHash('sha256').update(await readFile(join(repository,name))).digest('hex');
process.stdout.write(JSON.stringify({status:'passed',checked_at:new Date().toISOString(),platform:`${platform()} ${release()} ${arch()}`,cpu:cpus()[0]?.model,node:process.version,v8:process.versions.v8,three_revision:THREE.REVISION,fingerprints,scope:'file-backed trusted reference fixtures; adapter byte cache and CPU picking; excludes native desktop transport, DOM/GPU and visible response',cases},null,2)+'\n');
