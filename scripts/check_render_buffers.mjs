/** Actual viewer construction and Three allocation calls; excludes a real device. */
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {cpus,platform,release,arch} from 'node:os';
import {build} from 'esbuild';
import {renderBufferCounter} from './render-buffer-counter.mjs';

const repository=fileURLToPath(new URL('../',import.meta.url));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const stage=await fs.mkdtemp(path.join(repository,'target/render-buffer-evidence-'));
process.stderr.write(`Evidence: ${stage}\n`);
const vendor=path.join(repository,'node_modules/three-cad-viewer/dist/three-cad-viewer.esm.js');
await fs.writeFile(path.join(stage,'vendor.mjs'),await fs.readFile(vendor,'utf8')+'\nexport {NestedGroup};');
const {NestedGroup}=await import(pathToFileURL(path.join(stage,'vendor.mjs')).href);
const built=await build({entryPoints:[path.join(repository,'packages/viewer/src/render-buffers.ts'),path.join(repository,'packages/viewer/src/buffers.ts')],bundle:true,platform:'node',format:'esm',write:false,outdir:'unused'});
for(const output of built.outputFiles)await fs.writeFile(path.join(stage,path.basename(output.path).replace(/\.js$/,'.mjs')),output.text);
const {shareRenderBuffers,revisionRenderBuffers}=await import(pathToFileURL(path.join(stage,'render-buffers.mjs')).href);
const {sceneBuffers}=await import(pathToFileURL(path.join(stage,'buffers.mjs')).href);

async function measure(side){
  const directory=path.join(stage,`scene-${side}`);
  const prepared=spawnSync(path.join(repository,'.venv/bin/python'),[path.join(repository,'scripts/check_reference_scene.py'),'--grid-side',String(side),'--artifact-directory',directory],{encoding:'utf8',timeout:180000,maxBuffer:4*1024*1024});
  assert.equal(prepared.status,0,prepared.stderr);
  const inputReport=JSON.parse(prepared.stdout),model=JSON.parse(await fs.readFile(path.join(directory,'scene.json'),'utf8'));
  const descriptors=new Map(model.parts.flatMap(part=>Object.values(part.mesh.buffers).map(info=>[info.handle,info])));
  const load=sceneBuffers(async handle=>{const bytes=await fs.readFile(path.join(directory,handle));assert.equal(sha(bytes),descriptors.get(handle).sha256);return Uint8Array.from(bytes).buffer;});
  const parts=[];
  for(const [index,part]of model.parts.entries()){
    const buffers={};for(const [name,info]of Object.entries(part.mesh.buffers)){
      const data=await load(info);buffers[name]=['triangles','triangles_per_face','segments_per_edge','face_types','edge_types'].includes(name)?new Uint32Array(data):new Float32Array(data);
    }
    parts.push({version:3,id:`/model/${part.id}`,name:part.name,type:'shapes',subtype:'solid',shape:{...buffers,edge_types:Array.from(buffers.edge_types??[])},state:[1,1],color:index%2?'#e5ae69':'#b7c8d2',alpha:1,loc:[part.position,[0,0,0,1]]});
  }
  const tree={version:3,id:'/model',name:'Model',loc:[[0,0,0],[0,0,0,1]],parts};
  function trial(shared,fraction=1,fromEnd=false){
    const start=performance.now(),nested=new NestedGroup(tree,1600,1000,'#666666',false,1,.15,.65,0);nested.assignIds=true;nested.render();
    const construction_ms=performance.now()-start;
    const groups=parts.map(part=>nested.groups[part.id]);assert.equal(new Set(groups).size,parts.length);
    const ids=groups.map(group=>group.front.geometry.attributes.componentId);
    assert.equal(new Set(ids.map(attribute=>attribute.array.find(value=>value>0))).size,parts.length);
    const began=performance.now(),pool=shared?shareRenderBuffers(groups):null,pooling_ms=performance.now()-began;
    for(let i=0;i<groups.length;i++)assert.equal(groups[i].front.geometry.attributes.componentId,ids[i]);
    if(shared){assert.equal(groups[0].front.geometry.attributes.position,groups.at(-1).front.geometry.attributes.position);assert.equal(groups[0].front.geometry.index,groups.at(-1).front.geometry.index);}
    const device=renderBufferCounter(),count=Math.ceil(groups.length*fraction),drawn=fromEnd?groups.slice(-count):groups.slice(0,count);
    for(const group of drawn)device.draw(group);
    const allocated=device.snapshot(),sharing=pool?.stats??null;
    pool?.dispose();nested.dispose();const retired=device.snapshot();
    assert.equal(retired.live_bytes,0);assert.equal(retired.live_buffers,0);assert.equal(retired.registered_geometries,0);
    return {drawn_instances:drawn.length,construction_ms,pooling_ms,sharing,allocated,retired};
  }
  function revisions(reuse,dormant=false){
    const storage=reuse?revisionRenderBuffers():null,device=renderBufferCounter(),frames=[];
    let previous=null,previousPool=null,previousIds=[];
    for(let revision=0;revision<9;revision++){
      if(previous){storage?.beginSwap();previousPool.dispose();previous.dispose();}
      const nextTree={...tree,parts:parts.map((part,index)=>({...part,id:`/model/revision-${revision}-${index}`,loc:[[part.loc[0][0]+revision,part.loc[0][1],part.loc[0][2]],[0,0,0,1]]}))};
      const next=new NestedGroup(nextTree,1600,1000,'#666666',false,1,.15,.65,0);next.assignIds=true;next.render();
      const groups=nextTree.parts.map(part=>next.groups[part.id]),ids=groups.map(group=>group.front.geometry.attributes.componentId);
      for(let index=0;index<ids.length;index++)if(previousIds.length)assert.notEqual(ids[index],previousIds[index]);
      const pool=storage?storage.attach(groups):shareRenderBuffers(groups);storage?.finishSwap();
      const count=dormant&&revision%3===1?0:Math.ceil(groups.length*(dormant?.5:1));
      const drawn=revision%2?groups.slice(-count):groups.slice(0,count);
      // slice(-0) would draw everything in a nominally dormant revision.
      if(count)for(const group of drawn)device.draw(group);
      const state=storage?.stats??null;if(state)assert.ok(state.retainedBytes<=64*1024*1024);
      frames.push({revision,drawn_instances:count,retention:state,device:device.snapshot()});
      previous=next;previousPool=pool;previousIds=ids;
    }
    const allocated=device.snapshot();previousPool.dispose();previous.dispose();storage?.dispose();const retired=device.snapshot();
    assert.equal(retired.live_bytes,0);assert.equal(retired.live_buffers,0);assert.equal(retired.registered_geometries,0);
    return {frames,allocated,retired};
  }
  const baseline=trial(false),shared=trial(true);
  assert.ok(shared.allocated.upload_calls<baseline.allocated.upload_calls);
  assert.ok(shared.allocated.uploaded_bytes<baseline.allocated.uploaded_bytes);
  const swaps=[];for(let i=0;i<8;i++)swaps.push(trial(true,i%2?.5:1,i%4===3));
  const perScene=revisions(false),acrossRevisions=revisions(true),dormantRevisions=revisions(true,true);
  assert.ok(acrossRevisions.allocated.uploaded_bytes<perScene.allocated.uploaded_bytes);
  assert.ok(acrossRevisions.allocated.upload_calls<perScene.allocated.upload_calls);
  return {instances:parts.length,input_geometry:inputReport,baseline,shared,repeated_scenes:swaps,
    revision_uploads:{per_scene:perScene,retained:acrossRevisions,dormant:dormantRevisions,saved_upload_bytes:perScene.allocated.uploaded_bytes-acrossRevisions.allocated.uploaded_bytes},
    saved_upload_bytes:baseline.allocated.uploaded_bytes-shared.allocated.uploaded_bytes,
    saved_upload_fraction:1-shared.allocated.uploaded_bytes/baseline.allocated.uploaded_bytes};
}
const cases=[];for(const side of [10,20])cases.push(await measure(side));
const fingerprints={};for(const name of ['package-lock.json','packages/viewer/src/index.ts','packages/viewer/src/render-buffers.ts','scripts/render-buffer-counter.mjs','scripts/check_render_buffers.mjs','node_modules/three-cad-viewer/dist/three-cad-viewer.esm.js','node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLAttributes.js','node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLGeometries.js'])fingerprints[name]=sha(await fs.readFile(path.join(repository,name)));
const result={status:'passed',checked_at:new Date().toISOString(),platform:`${platform()} ${release()} ${arch()}`,cpu:cpus()[0]?.model,node:process.version,
  scope:'Pinned viewer geometry and pinned Three attribute allocation/disposal functions with a counting WebGL device; no actual GPU, renderer pixels, frame times, labels or desktop transport',
  evidence:stage,fingerprints,cases};
await fs.writeFile(path.join(stage,'experiment.json'),JSON.stringify(result,null,2)+'\n');
process.stdout.write(JSON.stringify(result,null,2)+'\n');
