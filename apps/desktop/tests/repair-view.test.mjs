// CPU-only adapter integration: real pinned groups/materials/camera APIs; no GL or app.
import test,{before,after,beforeEach,afterEach} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import ts from 'typescript';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import * as THREE from 'three';
const repo=fileURLToPath(new URL('../../../',import.meta.url));
let directory,dom,dependency,CadViewport,viewport,viewer,container,frames,selected,graphics;
const deferred=()=>{let resolve;const promise=new Promise(r=>resolve=r);return {promise,resolve};};
function model(alternate=false){
  const arrays={vertices:new Float32Array([0,0,0,10,0,0,0,10,0]),normals:new Float32Array([alternate?-0:0,0,1,0,0,1,0,0,1]),triangles:new Uint32Array([0,1,2]),triangles_per_face:new Uint32Array([1]),edges:new Float32Array([0,0,0,10,0,0]),segments_per_edge:new Uint32Array([1])};
  const data=new Map(),buffers=Object.fromEntries(Object.entries(arrays).map(([key,array])=>{
    const bytes=Buffer.from(array.buffer,array.byteOffset,array.byteLength),sha256=createHash('sha256').update(bytes).digest('hex'),handle=`${sha256}.bin`;
    data.set(handle,Uint8Array.from(bytes).buffer);return [key,{handle,bytes:bytes.length,sha256}];
  }));
  return {scene:{parts:['rail','carriage'].map((name,index)=>({id:name,instance_id:name,name,position:[index*20,0,0],bounds:{min:[0,0,0],max:[10,10,0],size:[10,10,0]},volume:null,area:50,mesh:{buffers,face_ids:['face:0'],edge_ids:['edge:0']}}))},artifact:async handle=>{assert.ok(data.has(handle));return data.get(handle);}};
}
function motion(workspace='workspace-1',x=40,version=1){return {workspace_id:workspace,revision:'r1',pose_version:version,mode:'manipulate',cad_units:'mm',collision_checked:true,joints:[],poses:{rail:{position:[x,0,0],quaternion:[0,0,0,1]},carriage:{position:[20,0,0],quaternion:[0,0,0,1]}}};}
const group=name=>viewer.nestedGroup.groups[`/model/${name}`];
function near(a,b){if(Array.isArray(a)){assert.equal(a.length,b.length);for(let i=0;i<a.length;i++)near(a[i],b[i]);}else assert.ok(Math.abs(a-b)<1e-9,`${a} != ${b}`);}
function cameraEqual(actual,expected){for(const key of ['position','quaternion','target','zoom'])near(actual[key],expected[key]);}

before(async()=>{
  directory=await fs.mkdtemp(path.join(repo,'target/repair-view-tests-'));
  const vendor=path.join(repo,'node_modules/three-cad-viewer/dist/three-cad-viewer.esm.js'),text=await fs.readFile(vendor,'utf8'),tree=ts.createSourceFile(vendor,text,ts.ScriptTarget.Latest,true);
  const names=new Set(['getCameraPosition','getCameraQuaternion','getCameraTarget','getCameraZoom','getCameraLocationSettings','setCameraLocationSettings','switchCamera']);
  const methods=[];let objectApi;
  function visit(node){
    if(ts.isClassDeclaration(node)&&node.name?.text==='Viewer')for(const member of node.members){
      if(member.name&&names.has(member.name.getText(tree)))methods.push(member.getText(tree));
      function arrows(n){if(ts.isBinaryExpression(n)&&ts.isPropertyAccessExpression(n.left)&&n.left.expression.kind===ts.SyntaxKind.ThisKeyword&&n.left.name.text==='setObject')objectApi=n.right.getText(tree);ts.forEachChild(n,arrows);}arrows(member);
    }ts.forEachChild(node,visit);
  }visit(tree);assert.equal(methods.length,names.size);assert.ok(objectApi);
  await fs.writeFile(path.join(directory,'vendor.mjs'),text+`\nexport {NestedGroup,Camera}; export const cameraApi={${methods.join(',')}}; export function objectApi(){return ${objectApi};}`);
  dependency=await import(pathToFileURL(path.join(directory,'vendor.mjs')).href);
  const compiled=await build({entryPoints:[path.join(repo,'packages/viewer/src/index.ts')],bundle:true,platform:'node',format:'esm',write:false,plugins:[{name:'cpu-host',setup(builder){
    builder.onResolve({filter:/^three-cad-viewer(?:\/css)?$/},args=>({path:args.path,namespace:'cpu-host'}));
    builder.onLoad({filter:/.*/,namespace:'cpu-host'},args=>({loader:'js',contents:args.path.endsWith('/css')?'':`export class Display{constructor(...args){return globalThis.__cadpadViewportHarness.display(...args);}} export class Viewer{constructor(...args){return globalThis.__cadpadViewportHarness.viewer(...args);}}`}));
  }}]});await fs.writeFile(path.join(directory,'adapter.mjs'),compiled.outputFiles[0].text);
  dom=new JSDOM('<main/>');for(const name of ['window','document','HTMLElement'])Object.defineProperty(globalThis,name,{value:dom.window[name],configurable:true,writable:true});
  globalThis.ResizeObserver=class{observe(){}disconnect(){}};
  ({CadViewport}=await import(pathToFileURL(path.join(directory,'adapter.mjs')).href));
});
beforeEach(()=>{
  frames=new Map();let nextFrame=0;selected=[];graphics=[];
  globalThis.requestAnimationFrame=callback=>{frames.set(++nextFrame,callback);return nextFrame;};globalThis.cancelAnimationFrame=id=>frames.delete(id);
  container=document.createElement('div');document.body.appendChild(container);Object.defineProperties(container,{clientWidth:{value:800},clientHeight:{value:600}});
  globalThis.__cadpadViewportHarness={
    display(host){const cadBody=document.createElement('div'),cadView=document.createElement('div');host.append(cadBody,cadView);return {cadBody,cadView,setSizes(){},resize(){globalThis.__cadpadViewportHarness.currentViewer?.resize();},showTools(){},showToolsPanel(){},showInfo(){},dispose(){cadBody.remove();cadView.remove();}};},
    viewer(_display,_options,notify){
      const scene=new THREE.Scene(),canvas=document.createElement('canvas'),state=new Map([['control','orbit'],['ortho',true]]);
      viewer={scene,state,renderer:{domElement:canvas,renderLists:{dispose(){}},setSize(width,height){this.size=[width,height];}},ready:false,rendered:null,isoCalls:0,failNext:false,
        get nestedGroup(){return this.rendered.nestedGroup;},get camera(){return this.rendered.camera;},get ortho(){return this.state.get('ortho');},
        hasAnimationLoop:false,toggleAnimationLoop(flag){this.hasAnimationLoop=flag;},idPicker:{setDirty(){}},_studioManager:{setCamera(){}},
        render(tree){
          if(this.failNext){this.failNext=false;throw new Error('Synthetic render setup failure');}
          const nestedGroup=new dependency.NestedGroup(tree,800,600,'#666',false,1,.15,.65,0);nestedGroup.assignIds=true;nestedGroup.render();
          const camera=new dependency.Camera(800,600,100,[0,0,0],true,'Z'),target=new THREE.Vector3();
          const controls={getTarget:()=>target,setTarget:value=>target.copy(value),setCamera(){},update(){camera.getCamera().lookAt(target);camera.getCamera().updateMatrixWorld(true);}};
          this.state.set('ortho',true);this.rendered={nestedGroup,camera,controls,gridHelper:{scaleLabels(){},update(){}}};this.ready=true;notify({target0:[0,0,0]});
        },
        update(){if(this.rendered){this.camera.getCamera().updateMatrixWorld(true);for(const object of Object.values(this.nestedGroup.groups))object.updateMatrixWorld(true);}},
        presetCamera(){this.isoCalls++;this.setCameraLocationSettings([100,100,100],null,[0,0,0],1);},
        showTools(){},glassMode(){},resize(){if(!this.ready||!this.rendered)throw new TypeError("Cannot read properties of null (reading 'camera')");},resizeCadView(width,tree,height,glass){if(!this.ready||!this.rendered)throw new TypeError("Cannot read properties of null (reading 'camera')");this.lastSize={width,tree,height,glass};},clear(){if(this.failClear){this.failClear=false;throw new Error('Synthetic clear failure');}this.rendered?.nestedGroup.dispose();this.rendered=null;this.ready=false;this.scene.clear();},dispose(){this.clear();},
        ...dependency.cameraApi,
      };viewer.setObject=dependency.objectApi.call(viewer);globalThis.__cadpadViewportHarness.currentViewer=viewer;return viewer;
    }
  };
  viewport=new CadViewport(container,entity=>selected.push(entity),undefined,state=>graphics.push(state));
});
afterEach(()=>{viewport.dispose();container.remove();delete globalThis.__cadpadViewportHarness;});
after(async()=>{dom.window.close();await fs.rm(directory,{recursive:true,force:true});});

for(const orthographic of [true,false])test(`diagonal cube views preserve ${orthographic?'orthographic':'perspective'} target, distance and zoom`,async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  viewer.switchCamera(orthographic);
  viewer.setCameraLocationSettings([130,90,160],null,[10,20,30],1.7);
  const before=viewer.getCameraLocationSettings();
  const distance=new THREE.Vector3(...before.position).distanceTo(new THREE.Vector3(...before.target));
  for(const x of [-1,0,1])for(const y of [-1,0,1])for(const z of [-1,0,1]){
    if([x,y,z].filter(Boolean).length<2)continue;
    viewport.view([x,y,z]);
    const after=viewer.getCameraLocationSettings();
    near(after.target,before.target);if(orthographic)near(after.zoom,before.zoom);
    const offset=new THREE.Vector3(...after.position).sub(new THREE.Vector3(...after.target));
    near(offset.length(),distance);
    const direction=new THREE.Vector3(x,y,z).normalize();
    near(offset.normalize().toArray(),direction.toArray());
    near(new THREE.Vector3(0,0,1).applyQuaternion(new THREE.Quaternion(...after.quaternion)).toArray(),direction.toArray());
  }
});

test('opening the workspace sizes the canvas to the container and does not touch an unready camera',async()=>{
  assert.equal(viewer.ready,false);
  assert.doesNotThrow(()=>viewport.setAdvancedTools(false));
  assert.equal(viewer.lastSize,undefined);
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  assert.deepEqual(viewer.lastSize,{width:800,tree:250,height:600,glass:true});
});

for(const orthographic of [true,false])test(`same revision repair preserves ${orthographic?'orthographic':'perspective'} camera, hidden materials, face selection and pose`,async()=>{
  const initial=model(),repair=model(true);await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  viewer.switchCamera(orthographic);viewer.setCameraLocationSettings([120,-80,60],null,[15,2,3],2.5);const camera=viewer.getCameraLocationSettings(),iso=viewer.isoCalls;
  viewport.setVisible('carriage',false);const entity={revision:'r1',instance_id:'rail',entity_id:'face:0'};viewport.selectEntity(entity);viewport.setMotion(motion());selected.length=0;
  await viewport.load(repair.scene,'r1',repair.artifact,'workspace-1');
  assert.equal(viewer.ortho,orthographic);cameraEqual(viewer.getCameraLocationSettings(),camera);assert.equal(viewer.isoCalls,iso);
  assert.equal(group('carriage').front.material.visible,false);assert.equal(group('carriage').edgeMaterial.visible,false);assert.equal(group('rail').front.material.visible,true);
  assert.deepEqual(viewport.selectedEntity,entity);assert.ok(viewport.highlights.length);assert.deepEqual(selected,[]);near(group('rail').position.toArray(),[40,0,0]);
  assert.equal(viewport.hasScene('workspace-1','r1'),true);
});

test('changes made while buffers are pending are captured at the actual replacement',async()=>{
  const initial=model(),repair=model(true),hold=deferred(),entered=deferred();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  const promise=viewport.load(repair.scene,'r1',async handle=>{if(handle===repair.scene.parts[0].mesh.buffers.normals.handle){entered.resolve();await hold.promise;}return repair.artifact(handle);},'workspace-1');
  await entered.promise;viewport.setVisible('rail',false);viewport.selectEntity({revision:'r1',instance_id:'carriage',entity_id:'edge:0'});viewer.setCameraLocationSettings([90,70,50],null,[20,1,2],3);
  const camera=viewer.getCameraLocationSettings();hold.resolve();await promise;
  assert.equal(group('rail').front.material.visible,false);assert.equal(viewport.selectedEntity.instance_id,'carriage');assert.equal(viewport.selectedEntity.entity_id,'edge:0');cameraEqual(viewer.getCameraLocationSettings(),camera);
});

test('cancelling a pending replacement retains the prior scoped scene',async()=>{
  const initial=model(),repair=model(true),hold=deferred(),entered=deferred();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewport.setVisible('carriage',false);
  const promise=viewport.load(repair.scene,'r2',async handle=>{entered.resolve();await hold.promise;return repair.artifact(handle);},'workspace-2');await entered.promise;viewport.cancelLoad();hold.resolve();await promise;
  assert.equal(viewport.hasScene('workspace-1','r1'),true);assert.equal(viewport.hasScene('workspace-2','r2'),false);assert.equal(group('carriage').front.material.visible,false);
});

test('new workspace or revision clears scoped selection, visibility and camera intent',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  for(const [workspace,revision] of [['workspace-2','r1'],['workspace-2','r2']]){
    viewport.setVisible('carriage',false);viewport.selectEntity({revision:viewport.revision,instance_id:'rail',entity_id:'face:0'});const iso=viewer.isoCalls;selected.length=0;
    await viewport.load(initial.scene,revision,initial.artifact,workspace);
    assert.equal(group('carriage').front.material.visible,true);assert.equal(viewport.selectedEntity,null);assert.deepEqual(selected,[null]);assert.equal(viewer.isoCalls,iso+1);
  }
});

test('a render failure retains logical repair state for retry and accepts newer canonical motion',async()=>{
  const initial=model(),repair=model(true);await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewport.setVisible('carriage',false);viewport.selectEntity({revision:'r1',instance_id:'rail',entity_id:'edge:0'});
  viewer.setCameraLocationSettings([90,70,50],null,[20,1,2],3);const camera=viewer.getCameraLocationSettings();viewer.failNext=true;
  await assert.rejects(viewport.load(repair.scene,'r1',repair.artifact,'workspace-1'),/Synthetic render setup failure/);assert.equal(viewport.hasScene('workspace-1','r1'),false);
  viewport.setMotion(motion('workspace-1',80,2));await viewport.load(repair.scene,'r1',repair.artifact,'workspace-1');
  cameraEqual(viewer.getCameraLocationSettings(),camera);assert.equal(group('carriage').front.material.visible,false);assert.equal(viewport.selectedEntity.entity_id,'edge:0');near(group('rail').position.toArray(),[80,0,0]);
});

test('same revision repair does not restart a paused simulation interpolation',async()=>{
  const initial=model(),repair=model(true);await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');const snapshot=motion();snapshot.mode='simulate';
  snapshot.simulation={timestep_s:1/120,previous:{tick:1,time_s:1/120,poses:motion('workspace-1',20).poses},current:{tick:2,time_s:2/120,poses:snapshot.poses}};
  viewport.setMotion(snapshot);for(const [id,callback] of [...frames]){frames.delete(id);callback(performance.now()+1000);}assert.equal(frames.size,0);
  await viewport.load(repair.scene,'r1',repair.artifact,'workspace-1');viewport.setMotion(snapshot);
  assert.equal(frames.size,0);near(group('rail').position.toArray(),[40,0,0]);
});

test('unavailable topology is cleared instead of selecting a different tessellation index',async()=>{
  const initial=model(),repair=model(true);await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewport.selectEntity({revision:'r1',instance_id:'rail',entity_id:'face:0'});selected.length=0;
  repair.scene.parts[0].mesh.face_ids=['face:1'];
  await viewport.load(repair.scene,'r1',repair.artifact,'workspace-1');
  assert.equal(viewport.selectedEntity,null);assert.deepEqual(selected,[null]);assert.equal(viewport.highlights.length,0);
});

test('motion from a different workspace cannot move a scene with the same revision text',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-2');viewport.setMotion(motion('workspace-1',90));
  assert.equal(viewport.motion,null);near(group('rail').position.toArray(),[0,0,0]);
  viewport.setMotion(motion('workspace-2',60));near(group('rail').position.toArray(),[60,0,0]);
});

test('failure while clearing a scene releases prepared picking owners and can be retried',async()=>{
  const initial=model(),repair=model(true);await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewport.setVisible('carriage',false);
  const pool=viewport.pickingPool,prepared=[];viewport.pickingPool=(...args)=>{const item=pool(...args);prepared.push(item);return item;};viewer.failClear=true;
  await assert.rejects(viewport.load(repair.scene,'r1',repair.artifact,'workspace-1'),/Synthetic clear failure/);
  assert.equal(prepared.length,2);assert.ok(prepared.every(item=>item.disposed&&item.resources.released));assert.equal(viewport.hasScene('workspace-1','r1'),false);
  await viewport.load(repair.scene,'r1',repair.artifact,'workspace-1');assert.equal(viewport.hasScene('workspace-1','r1'),true);assert.equal(group('carriage').front.material.visible,false);
});

function graphicsEvent(type){const event=new dom.window.Event(`webglcontext${type}`,{cancelable:true});viewer.renderer.domElement.dispatchEvent(event);return event;}

test('context loss invalidates capture, cancels animation and restores logical state only after reload',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  viewer.switchCamera(false);viewer.setCameraLocationSettings([120,-80,60],null,[15,2,3],2.5);const camera=viewer.getCameraLocationSettings();
  viewport.setVisible('carriage',false);const entity={revision:'r1',instance_id:'rail',entity_id:'face:0'};viewport.selectEntity(entity);
  const snapshot=motion();snapshot.mode='simulate';snapshot.simulation={timestep_s:1/120,previous:{tick:1,time_s:1/120,poses:motion('workspace-1',20).poses},current:{tick:2,time_s:2/120,poses:snapshot.poses}};
  viewport.setMotion(snapshot);assert.ok(frames.size>0);
  viewer.hasAnimationLoop=true;assert.equal(graphicsEvent('lost').defaultPrevented,true);assert.equal(viewer.hasAnimationLoop,false);graphicsEvent('lost');
  assert.deepEqual(graphics,['lost']);assert.equal(viewport.hasScene('workspace-1','r1'),false);assert.equal(frames.size,0);assert.equal(viewport.highlights.length,0);
  assert.throws(()=>viewport.capture({}),/No committed geometry/);
  let fetched=false;await assert.rejects(viewport.load(initial.scene,'r1',async()=>{fetched=true;throw new Error('Unexpected fetch');},'workspace-1'),/graphics connection is unavailable/);assert.equal(fetched,false);
  viewport.setMotion(motion('workspace-1',80,2));
  graphicsEvent('restored');graphicsEvent('restored');assert.deepEqual(graphics,['lost','restored']);assert.equal(viewport.hasScene('workspace-1','r1'),false,'The event itself cannot certify a displayed scene');
  await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  assert.equal(viewport.hasScene('workspace-1','r1'),true);assert.equal(viewer.ortho,false);cameraEqual(viewer.getCameraLocationSettings(),camera);
  assert.equal(group('carriage').front.material.visible,false);assert.deepEqual(viewport.selectedEntity,entity);near(group('rail').position.toArray(),[80,0,0]);assert.equal(frames.size,0);
});

test('context loss cancels a pending scene so it cannot commit after restoration',async()=>{
  const initial=model(),next=model(true),hold=deferred(),entered=deferred();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  const pending=viewport.load(next.scene,'r2',async handle=>{entered.resolve();await hold.promise;return next.artifact(handle);},'workspace-2');await entered.promise;
  graphicsEvent('lost');graphicsEvent('restored');await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');hold.resolve();await pending;
  assert.equal(viewport.hasScene('workspace-1','r1'),true);assert.equal(viewport.hasScene('workspace-2','r2'),false);
});

test('loss during renderer setup cannot publish a ready scene and listeners are removed at disposal',async()=>{
  const initial=model(),render=viewer.render;viewer.render=function(...args){render.apply(this,args);graphicsEvent('lost');};
  await assert.rejects(viewport.load(initial.scene,'r1',initial.artifact,'workspace-1'),/graphics connection was interrupted/);
  assert.equal(viewport.hasScene('workspace-1','r1'),false);graphicsEvent('restored');viewer.render=render;await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');assert.equal(viewport.hasScene('workspace-1','r1'),true);
  viewport.dispose();const count=graphics.length;graphicsEvent('lost');graphicsEvent('restored');assert.equal(graphics.length,count);
});

test('loss during capture rejects the image and retains the original user camera and dimension preferences',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewer.switchCamera(false);viewer.setCameraLocationSettings([90,70,50],null,[20,1,2],3);const camera=viewer.getCameraLocationSettings();
  viewport.setDimensions(true,'selected');const update=viewer.update;let lose=true;viewer.update=function(...args){update.apply(this,args);if(lose){lose=false;graphicsEvent('lost');}};
  assert.throws(()=>viewport.capture({camera_preset:'front',dimensions:false}),error=>error.code==='VIEW_NOT_READY'&&/graphics connection/.test(error.message));
  assert.equal(viewport.dimensions,true);assert.equal(viewport.scope,'selected');viewer.update=update;
  graphicsEvent('restored');await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  assert.equal(viewer.ortho,false);cameraEqual(viewer.getCameraLocationSettings(),camera);assert.equal(viewport.dimensions,true);assert.equal(viewport.scope,'selected');
});

test('missing cache after graphics loss retains logical scene intent while readiness stays false',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');viewer.setCameraLocationSettings([90,70,50],null,[20,1,2],3);const camera=viewer.getCameraLocationSettings();viewport.setVisible('carriage',false);const entity={revision:'r1',instance_id:'rail',entity_id:'face:0'};viewport.selectEntity(entity);
  graphicsEvent('lost');assert.deepEqual(viewport.sceneState('workspace-1','r1'),{selection:entity,hidden:['carriage']});assert.equal(viewport.sceneState('workspace-2','r1'),null);assert.equal(viewport.hasScene('workspace-1','r1'),false);
  graphicsEvent('restored');assert.deepEqual(viewport.sceneState('workspace-1','r1'),{selection:entity,hidden:['carriage']});assert.equal(viewport.hasScene('workspace-1','r1'),false);
  // The app waits for explicit cache restoration, then reloads this same model.
  await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');cameraEqual(viewer.getCameraLocationSettings(),camera);assert.deepEqual(viewport.selectedEntity,entity);assert.equal(group('carriage').front.material.visible,false);
});

for(const stage of ['display','viewer','observe'])test(`startup rollback owns its DOM and releases acquired resources after ${stage} failure`,async()=>{
  viewport.dispose();const host=globalThis.__cadpadViewportHarness,display=host.display,createViewer=host.viewer,observer=globalThis.ResizeObserver,failure=new Error(`Startup ${stage} failure`),marker=document.createElement('p');container.append(marker);
  let displayDisposals=0,viewerDisposals=0,disconnects=0,failedCanvas;
  host.display=(...args)=>{if(stage==='display'){args[0].appendChild(document.createElement('canvas'));throw failure;}const value=display(...args),dispose=value.dispose;value.dispose=()=>{displayDisposals++;dispose();};return value;};
  host.viewer=(...args)=>{if(stage==='viewer')throw failure;const value=createViewer(...args),dispose=value.dispose;failedCanvas=value.renderer.domElement;value.dispose=()=>{viewerDisposals++;dispose.call(value);};return value;};
  globalThis.ResizeObserver=class{observe(){throw failure;}disconnect(){disconnects++;}};
  try{
    assert.throws(()=>new CadViewport(container,()=>{},undefined,state=>graphics.push(state)),error=>error===failure);
    assert.deepEqual([...container.children],[marker]);assert.equal(displayDisposals,stage==='display'?0:1);assert.equal(viewerDisposals,stage==='observe'?1:0);assert.equal(disconnects,stage==='observe'?1:0);
    if(failedCanvas){const count=graphics.length;failedCanvas.dispatchEvent(new dom.window.Event('webglcontextlost',{cancelable:true}));assert.equal(graphics.length,count,'A failed startup must detach its graphics callbacks');}
  }finally{host.display=display;host.viewer=createViewer;globalThis.ResizeObserver=observer;}
  viewport=new CadViewport(container,entity=>selected.push(entity));const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');assert.equal(viewport.hasScene('workspace-1','r1'),true);assert.equal(container.querySelectorAll('.cadpad-viewer-host').length,1);assert.ok(marker.isConnected);
});

test('startup rollback attempts remaining cleanup and preserves the original failure if a disposer throws',()=>{
  viewport.dispose();const host=globalThis.__cadpadViewportHarness,display=host.display,createViewer=host.viewer,observer=globalThis.ResizeObserver,failure=new Error('Startup observer failure');let displayDisposed=false,disconnected=false;
  host.display=(...args)=>{const value=display(...args),dispose=value.dispose;value.dispose=()=>{displayDisposed=true;dispose();};return value;};
  host.viewer=(...args)=>{const value=createViewer(...args);value.dispose=()=>{throw new Error('Secondary cleanup failure');};return value;};
  globalThis.ResizeObserver=class{observe(){throw failure;}disconnect(){disconnected=true;}};
  try{assert.throws(()=>new CadViewport(container,()=>{}),error=>error===failure);assert.equal(displayDisposed,true);assert.equal(disconnected,true);assert.equal(container.children.length,0);}
  finally{host.display=display;host.viewer=createViewer;globalThis.ResizeObserver=observer;viewport=new CadViewport(container,()=>{});}
});

test('move mode picks whole parts with edge inspection active and never passes a missed primary drag to the camera',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  const canvas=viewer.renderer.domElement;
  viewport.host.appendChild(canvas);
  canvas.getBoundingClientRect=()=>({left:0,top:0,width:800,height:600});
  Object.defineProperty(canvas,'clientHeight',{value:600});
  container.setPointerCapture=()=>{};container.hasPointerCapture=()=>false;
  const snapshot=motion('workspace-1',0,1);
  snapshot.joints=[{id:'slide',parent:'rail',child:'carriage',kind:'prismatic',coordinate:.02,unit:'m',limits:[0,.1],axis_world:[1,0,0],origin_mm:[0,0,0]}];
  viewport.setMotion(snapshot);viewport.setSelectionMode('edge');
  const targets=[];viewport.setManipulation(true,(...args)=>targets.push(args));
  const project=(x,y)=>{const p=new THREE.Vector3(x,y,0).project(viewer.camera.getCamera());return {clientX:(p.x+1)*400,clientY:(1-p.y)*300};};
  const pointer=(type,point)=>{
    const event=new dom.window.MouseEvent(type,{bubbles:true,cancelable:true,button:0,...point});Object.defineProperty(event,'pointerId',{value:1});
    canvas.dispatchEvent(event);return event;
  };
  const start=project(23,3),end=project(33,3);
  const marker=container.querySelector('.canvas-press-marker');assert.equal(marker.hidden,true);
  const down=pointer('pointerdown',start);
  assert.equal(marker.hidden,false);assert.equal(marker.dataset.state,'moving');
  const origin=marker.style.left;
  assert.equal(down.defaultPrevented,true);
  assert.equal(viewport.drag?.joint.id,'slide');
  assert.equal(selected.at(-1).entity_id,'part:0');
  pointer('pointermove',end);assert.equal(marker.style.left,origin,'The press marker stays at the registered start point');
  pointer('pointerup',end);assert.equal(marker.hidden,true);
  assert.ok(targets.some(([id,value,,commit])=>id==='slide'&&Math.abs(value-.03)<1e-8&&commit));
  let cameraStarts=0;const cameraListener=()=>cameraStarts++;
  container.addEventListener('pointerdown',cameraListener);
  assert.equal(pointer('pointerdown',{clientX:-1000,clientY:-1000}).defaultPrevented,true);
  assert.equal(cameraStarts,0);
  assert.equal(viewport.drag,null);
  assert.equal(pointer('pointerdown',project(3,3)).defaultPrevented,true);
  assert.equal(canvas.style.cursor,'not-allowed');assert.equal(marker.dataset.state,'blocked');
  pointer('pointercancel',start);assert.equal(marker.hidden,true);
  viewport.setManipulation(false,()=>{});
  pointer('pointerdown',start);assert.equal(cameraStarts,1);assert.equal(marker.dataset.state,'pressed');
  pointer('pointerup',start);assert.equal(marker.hidden,true);
  container.removeEventListener('pointerdown',cameraListener);
});

test('hover metadata moves to the external status line and retains live text updates',()=>{
  const line=document.createElement('div');line.className='tcv_status_line';viewport.host.appendChild(line);
  const status=document.createElement('span');document.body.appendChild(status);
  viewport.setStatusContainer(status);
  line.textContent='Carriage · planar face · 120 mm²';
  assert.equal(status.textContent,'Carriage · planar face · 120 mm²');
  assert.equal(viewport.host.querySelector('.tcv_status_line'),null);
  line.textContent='';line.style.display='none';assert.equal(status.textContent,'');
  status.remove();
});

test('move mode persistently marks draggable geometry and explains fixed parts in the external status line',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  const snapshot=motion('workspace-1',0,1);snapshot.joints=[{id:'slide',parent:'rail',child:'carriage',kind:'prismatic',coordinate:.02,unit:'m',limits:[0,.1],axis_world:[1,0,0],origin_mm:[0,0,0]}];
  viewport.setMotion(snapshot);
  const status=document.createElement('span');document.body.appendChild(status);viewport.setStatusContainer(status);
  viewport.setManipulation(true,()=>{});
  const hints=viewport.highlights.filter(item=>item.kind==='motion');assert.ok(hints.length);
  assert.ok(hints.every(item=>item.object.parent===group('carriage')));
  viewport.updateMotionStatus({revision:'r1',instance_id:'rail',entity_id:'part:0'});assert.match(status.textContent,/rail.*Fixed.*cannot be dragged/);
  viewport.updateMotionStatus({revision:'r1',instance_id:'carriage',entity_id:'part:0'});assert.match(status.textContent,/carriage.*Drag to slide/);
  viewport.setVisible('carriage',false);assert.equal(viewport.highlights.filter(item=>item.kind==='motion').length,0);
  viewport.setVisible('carriage',true);assert.ok(viewport.highlights.some(item=>item.kind==='motion'));
  viewport.setManipulation(false,()=>{});assert.equal(viewport.highlights.filter(item=>item.kind==='motion').length,0);assert.equal(viewport.motionStatusElement.hidden,true);
  status.remove();
});

test('replaying unchanged assembly visibility causes no scene paints',async()=>{
  const initial=model();await viewport.load(initial.scene,'r1',initial.artifact,'workspace-1');
  let paints=0;const update=viewer.update.bind(viewer);viewer.update=(...args)=>{paints++;return update(...args);};
  for(let i=0;i<100;i++)for(const part of initial.scene.parts)viewport.setVisible(part.instance_id,true);
  assert.equal(paints,0);
  viewport.setVisible('rail',false);assert.ok(paints>0);
  const hiddenPaints=paints;viewport.setVisible('rail',false);assert.equal(paints,hiddenPaints);
  viewport.setVisible('rail',true);assert.ok(paints>hiddenPaints);
  const visiblePaints=paints;viewport.setVisible('rail',true);assert.equal(paints,visiblePaints);
});


test('hidden tabs release mesh arrays and picking/render resources while preserving interaction state',async()=>{
  const initial=model();
  const artifact=async handle=>(await initial.artifact(handle)).slice(0);
  for(let cycle=0;cycle<24;cycle++){
    await viewport.load(initial.scene,'r1',artifact,'workspace-1');
    viewer.setCameraLocationSettings([130,90,160],null,[10,20,30],1.7);
    viewport.setVisible('carriage',false);
    viewport.selectEntity({revision:'r1',instance_id:'rail',entity_id:'face:0'});
    const camera=viewer.getCameraLocationSettings();
    assert.ok(viewport.pickGeometry.size>0);
    assert.ok(viewport.revisionBytes.stats.bytes>0);
    viewport.suspend();
    assert.equal(viewer.ready,false);
    assert.equal(viewer.rendered,null);
    assert.equal(viewport.pickGeometry.size,0);
    assert.deepEqual(viewport.revisionBytes.stats,{entries:0,bytes:0});
    assert.equal(viewport.renderStorage,null);
    assert.equal(viewport.renderedBuffers,null);
    assert.deepEqual(viewer.renderer.size,[1,1]);
    assert.equal(frames.size,0);
    assert.deepEqual(viewport.sceneState('workspace-1','r1'),{hidden:['carriage'],selection:{revision:'r1',instance_id:'rail',entity_id:'face:0'}});
    await viewport.load(initial.scene,'r1',artifact,'workspace-1');
    cameraEqual(viewer.getCameraLocationSettings(),camera);
    assert.equal(viewport.hasScene('workspace-1','r1'),true);
    assert.equal(group('carriage').front.material.visible,false);
    viewport.suspend();
  }
});

test('suspending a pending tab rejects late geometry and keeps its caches empty',async()=>{
  const initial=model(),pending=deferred();
  const loading=viewport.load(initial.scene,'r1',async handle=>{await pending.promise;return initial.artifact(handle);},'workspace-1');
  viewport.suspend();pending.resolve();await loading;
  assert.equal(viewer.ready,false);
  assert.equal(viewport.pickGeometry.size,0);
  assert.deepEqual(viewport.revisionBytes.stats,{entries:0,bytes:0});
  assert.equal(viewport.hasScene('workspace-1','r1'),false);
});


test('suspended mesh ArrayBuffers can be garbage collected', {skip:typeof globalThis.gc!=='function'}, async()=>{
  const initial=model();
  await viewport.load(initial.scene,'r1',async handle=>(await initial.artifact(handle)).slice(0),'workspace-1');
  const bytes=new WeakRef(viewport.pickGeometry.values().next().value.buffers.vertices.buffer);
  viewport.suspend();
  let released=false;
  for(let attempt=0;attempt<12;attempt++){
    await new Promise(resolve=>setImmediate(resolve));
    globalThis.gc();
    if(!bytes.deref()){released=true;break;}
  }
  assert.ok(released,'the suspended viewer must not retain typed array storage through caches, geometry or callbacks');
});
