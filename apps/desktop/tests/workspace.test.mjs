// CPU-only React interaction checks. No browser, WebGL, native app or provider.
import test,{after,before,beforeEach,afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {fileURLToPath,pathToFileURL} from 'node:url';
import path from 'node:path';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import React,{act} from 'react';

const repo=fileURLToPath(new URL('../../../',import.meta.url));
let directory,App,Workbench,docking,createRoot,dom,root,harness;
const fixture=()=>({workspace_id:'workspace-1',revision:'r1',name:'Rail assembly',dirty:true,saved_revision:null,external_tracking:false,activity:[],recordings:[],
  files:['model.py','parts/rail.py','parts/carriage.py'],assets:{'bracket.step':'abc123fingerprint'},entry_point:'model.py',
  parameters:{rail_length:{value:200,unit:'mm',minimum:140,maximum:1000,description:'Length of the rail'}},
  model:{parts:['rail','carriage'].map(name=>({id:name,instance_id:name,name,position:[0,0,0],bounds:{min:[0,0,0],max:[200,30,10],size:[200,30,10]},volume:6000,area:1000,mesh:{buffers:{},face_ids:[],edge_ids:[]}})),annotations:[{id:'mounting_spacing',kind:'distance',part:'rail',start:[-60,0,5],end:[60,0,5],value:120,label:'Mount spacing'}]},
  motion:{workspace_id:'workspace-1',revision:'r1',pose_version:0,actor:'local-user',mode:'manipulate',joints:[{id:'slide',kind:'prismatic',unit:'m',coordinate:.05,limits:[0,.15],drivable:true}],poses:{},collision:{clearance_m:.001,ignored_pairs:[]},collision_checked:true}});
const active=element=>!element.closest('[hidden],dialog:not([open])')&&!element.closest('details:not([open])');
const button=text=>[...document.querySelectorAll('button')].find(b=>b.textContent.trim()===text&&active(b));
async function click(element){assert.ok(element,'Expected control to exist');await act(async()=>{element.click();await new Promise(resolve=>setTimeout(resolve,5));});}
const motionIcon={Move:'tab-motion',Physics:'tab-physics',Requirements:'tab-tests'};
async function chooseMotionTask(title){const icon=document.getElementById(motionIcon[title]);if(icon.getAttribute('aria-expanded')!=='true')await click(icon);}
async function openMotion(){const panel=document.getElementById('panel-motion');if(panel.hidden)await chooseMotionTask(panel.getAttribute('aria-label'));}
async function openMotionSection(title){await click([...document.querySelectorAll('#panel-motion .motion-disclosure>summary')].find(summary=>summary.textContent.startsWith(title)));}
async function flush(ms=5){await act(async()=>{await new Promise(resolve=>setTimeout(resolve,ms));});}
async function setValue(element,value,event='input'){
  await act(async()=>{
    const proto=element instanceof dom.window.HTMLTextAreaElement?dom.window.HTMLTextAreaElement.prototype:element instanceof dom.window.HTMLSelectElement?dom.window.HTMLSelectElement.prototype:dom.window.HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto,'value').set.call(element,value);
    element.dispatchEvent(new dom.window.Event(event,{bubbles:true}));await new Promise(resolve=>setTimeout(resolve,10));
  });
}

before(async()=>{
  await mkdir(path.join(repo,'target'),{recursive:true});directory=await mkdtemp(path.join(repo,'target/ui-tests-'));
  const result=await build({stdin:{contents:`export {App} from './app';export {Workbench} from './workbench';export * from './dock-layout';export {buildErrorInfo} from './build-diagnostics';export {dockEdgeAt} from './dock-drag';`,resolveDir:path.join(repo,'apps/desktop/src'),loader:'tsx'},bundle:true,platform:'node',format:'esm',jsx:'automatic',write:false,loader:{'.css':'empty'},external:['react','react/jsx-runtime','react-dom'],plugins:[{
    name:'isolated-native-and-webgl',setup(builder){
      builder.onResolve({filter:/^(@cadpad\/viewer|@tauri-apps\/api\/core)$/},args=>({path:args.path,namespace:'harness'}));
      builder.onLoad({filter:/.*/,namespace:'harness'},args=>({loader:'js',resolveDir:repo,contents:args.path.startsWith('@tauri')?
        `export class Channel{onmessage=null;} export const invoke=(...args)=>globalThis.__cadpadUiHarness.invoke(...args);`:
        `export {sceneEntity,sameEntity,referenceLetter} from ${JSON.stringify(path.join(repo,'packages/viewer/src/entities.ts'))};
         export {selectedAnchor,selectedCircle} from ${JSON.stringify(path.join(repo,'packages/viewer/src/references.ts'))};
         export {recordedFrameAt,partMotion} from ${JSON.stringify(path.join(repo,'packages/viewer/src/motion.ts'))};
         export {dimensionText} from ${JSON.stringify(path.join(repo,'packages/viewer/src/dimension-display.ts'))};
         export const modelDimensions=model=>model.annotations||[];
         export class CadViewport{constructor(container,onSelect,onAnnotation,onGraphics){const host=globalThis.__cadpadUiHarness;host.constructions=(host.constructions||0)+1;(host.viewports??=[]).push(this);if(host.startupFailures){host.startupFailures--;throw new Error('Synthetic WebGL initialization failure');}globalThis.__cadpadUiHarness.graphics=state=>{if(state==='lost'){this.presented=null;this.cancelLoad();}onGraphics(state);};this.onSelect=onSelect;this.hidden=new Set();this.selection=null;globalThis.__cadpadUiHarness.viewport=this;globalThis.__cadpadUiHarness.select=entity=>{this.selection=entity;onSelect(entity);};globalThis.__cadpadUiHarness.annotation=onAnnotation;}
           async load(...args){const sequence=this.sequence=(this.sequence||0)+1;this.call('load',...args);await globalThis.__cadpadUiHarness.load?.(...args);if(sequence===this.sequence){if(!args[0].parts.length||!this.sceneState(args[3],args[1])){this.hidden.clear();this.selection=null;this.onSelect(null,'replacement');}this.logical=args[0].parts.length?{workspace:args[3],revision:args[1]}:null;this.presented=this.logical;}}
           hasScene(workspace,revision){return this.presented?.workspace===workspace&&this.presented?.revision===revision;} sceneState(workspace,revision){return this.logical?.workspace===workspace&&this.logical?.revision===revision?{hidden:[...this.hidden],selection:this.selection}:null;} cancelLoad(){this.sequence=(this.sequence||0)+1;} call(name,...args){globalThis.__cadpadUiHarness.viewer.push([name,...args]);}
           setStatusContainer(...a){this.call('statusContainer',...a);} setDimensions(...a){this.call('dimensions',...a);} setMotion(...a){} setSelectionMode(...a){this.call('selectionMode',...a);} setManipulation(...a){this.call('manipulation',...a);} dispose(){this.cancelLoad();this.presented=null;this.logical=null;this.call('dispose');}
           selectEntity(entity){this.selection=entity;this.call('selectEntity',entity);} setReferences(...a){this.references=a[0];this.call('references',...a);} select(...a){this.selection=a[0]?{revision:this.presented?.revision,instance_id:a[0],entity_id:'part:0'}:null;this.call('select',...a);} setVisible(...a){a[1]?this.hidden.delete(a[0]):this.hidden.add(a[0]);this.call('visible',...a);} view(...a){this.call('view',...a);} cameraQuaternion(){return [0,0,0,1];} capture(){globalThis.__cadpadUiHarness.captureCalls=(globalThis.__cadpadUiHarness.captureCalls||0)+1;throw new Error('Unexpected fixture capture');} resetView(){this.call('fit');} setAdvancedTools(...a){this.call('advanced',...a);} previewCoordinate(...a){this.call('preview',...a);}
         }`}));
    }
  }]});
  const file=path.join(directory,'app.mjs');await writeFile(file,result.outputFiles[0].text);
  // React DOM feature detection must run after the DOM has been installed.
  dom=new JSDOM('<div id="root"></div>',{url:'https://cadpad.test'});
  for(const key of ['window','document','HTMLElement','HTMLDialogElement','Node','Event','MouseEvent','navigator','localStorage'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true,writable:true});
  if(!dom.window.Element.prototype.scrollIntoView)dom.window.Element.prototype.scrollIntoView=function(){};
  globalThis.IS_REACT_ACT_ENVIRONMENT=true;
  globalThis.requestAnimationFrame=callback=>setTimeout(()=>callback(performance.now()),1);globalThis.cancelAnimationFrame=clearTimeout;
  dom.window.HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  dom.window.HTMLDialogElement.prototype.close=function(){this.open=false;this.dispatchEvent(new dom.window.Event('close'));};
  ({createRoot}=await import('react-dom/client'));(docking=await import(pathToFileURL(file).href),{App,Workbench}=docking);
});
beforeEach(async()=>{
  localStorage.clear();
  const project=fixture();harness={viewer:[],calls:[],project,async invoke(command,args){
    this.calls.push([command,args]);
    if(command==='connect_native_menu'){this.menu=args.channel;return;}
    if(command==='connect_activity'){this.activity=args.channel;return;}
    if(command==='connect_agent'){this.agent=args.channel;return;}
    if(command==='initialize'||command==='project'||command==='save_project')return structuredClone(project);
    if(command==='read_source')return {path:args.path,source:`# fixture ${args.path}\nwidth = 42\n`,hash:'fixture'};
    if(command==='workspaces')return [{workspace_id:project.workspace_id,name:project.name,running:0}];
    if(command==='agent_conversation')return {messages:[],turn:null};
    if(command==='agent_settings')return {provider:'fixture',model:'test-model',configured:true,credential_storage:'fixture'};
    if(command==='mcp_settings')return {config:{},bridge_installed:true,sessions:[]};
    if(command==='motion_tests')return {catalog:'fixture-catalog',tests:[],accuracy:'Synthetic test fixture',snapshot:null};
    if(command==='inspect')return {revision:'r1',pose_version:0,entities:[{reference:args.args.entities[0],kind:'part',volume:6000,area:1000,bounds:{size:[200,30,10]}}]};
    if(command==='joint_coordinate')return {snapshot:{...project.motion,pose_version:1,joints:[{...project.motion.joints[0],coordinate:args.request.coordinate}]}};
    if(command==='agent_start')return {id:'turn-1',workspace_id:project.workspace_id,state:'succeeded',text:'Done',activity:'Ready',artifacts:[]};
    if(command==='connect_view'){this.view=args.channel;return 'view-token';}
    return {id:'job-1',state:'succeeded',diagnostics:{}};
  }};globalThis.__cadpadUiHarness=harness;
  root=createRoot(document.getElementById('root'));await act(async()=>{root.render(React.createElement(App));});await flush();
});
afterEach(async()=>{
  await flush();
  await act(async()=>root.unmount());
  harness.viewer.length=0;harness.calls.length=0;harness.viewports=[];
  globalThis.__cadpadUiHarness=harness;
});
after(async()=>{dom?.window.close();await rm(directory,{recursive:true,force:true});delete globalThis.__cadpadUiHarness;});

test('Explorer shows project parts and assets without exposing Python source',async()=>{
  const panel=document.getElementById('panel-model');
  assert.equal(panel.getAttribute('aria-label'),'Explorer');
  assert.ok(!panel.querySelector('.explorer-project'),'Explorer does not repeat the document title');
  assert.doesNotMatch(panel.textContent,/Rail assembly/);
  assert.equal(panel.querySelector('.explorer-parts').closest('details').querySelector('summary').textContent,'Parts 2');
  assert.doesNotMatch(panel.textContent,/model\.py|rail\.py|# fixture|Source/);
  assert.match(panel.textContent,/bracket\.step/);
  assert.equal(panel.querySelectorAll('.part-row').length,2);
  assert.ok(panel.querySelector('.parameter').closest('.explorer-model'),'Parameters belong to Model');
  assert.ok(!panel.querySelector('.explorer-assets').closest('.explorer-model'),'Imported assets sit alongside Model');
  const search=panel.querySelector('[aria-label="Find parts or assets"]');
  await setValue(search,'carriage');
  assert.equal(panel.querySelectorAll('.part-row').length,1);
  await click(panel.querySelector('.part-select'));
  assert.ok(harness.viewer.some(([name,id])=>name==='select'&&id==='carriage'));
  await setValue(search,'not a part');
  assert.match(panel.textContent,/No matching parts or assets/);
  await setValue(search,'');
  assert.equal(panel.querySelectorAll('.part-row').length,2);
  assert.equal(harness.calls.some(([name])=>name==='read_source'),false);
});
test('model inspector preserves part selection, exact properties and visibility actions',async()=>{
  assert.ok(harness.viewer.some(call=>call[0]==='load'&&call[2]===harness.project.revision&&call[4]===harness.project.workspace_id),'revision-byte reuse must receive the current workspace scope');
  assert.equal(document.getElementById('panel-model').hidden,false);
  await click(button('rail'));await flush(180);
  assert.ok(harness.viewer.some(call=>call[0]==='select'&&call[1]==='rail'));
  assert.match(document.querySelector('.property-grid').textContent,/6[,.]000/);
  assert.equal(document.querySelector('.technical-details').open,false);
  await click(document.querySelector('[aria-label="Hide rail"]'));
  assert.ok(harness.viewer.some(call=>call[0]==='visible'&&call[1]==='rail'&&call[2]===false));
  assert.ok(document.querySelector('[aria-label="Show rail"]'));
});
test('view cube faces, edges and corners select all 26 camera directions',async()=>{
  const cube=document.querySelector('.view-cube');
  const targets=[...cube.querySelectorAll('.view-cube-target')];
  assert.equal(targets.length,54,'Each face has a center, four edges and four corners');
  const distinct=new Map(targets.map(target=>[target.getAttribute('aria-label'),target]));
  assert.equal(distinct.size,26);
  await click(distinct.get('Front view'));
  assert.equal(harness.viewer.findLast(([name])=>name==='view')[1],'front');
  await click(distinct.get('Top Front Right view'));
  assert.deepEqual(harness.viewer.findLast(([name])=>name==='view')[1],[1,-1,1]);
  await click(distinct.get('Bottom Back Left view'));
  assert.deepEqual(harness.viewer.findLast(([name])=>name==='view')[1],[-1,1,-1]);
  await click(distinct.get('Top Right view'));
  assert.deepEqual(harness.viewer.findLast(([name])=>name==='view')[1],[1,0,1]);
  await click(document.querySelector('[aria-label="Fit model in view"]'));
  assert.ok(harness.viewer.some(([name])=>name==='fit'));
});
test('toolbar selection and dimension scope use native checked menus',async()=>{
  assert.equal(document.querySelector('.viewport-tools select'),null);
  assert.equal(button('File'),undefined);
  await click(document.querySelector('[aria-label="Selection kind"]'));
  assert.equal(harness.calls.findLast(([name])=>name==='popup_native_menu')[1].kind,'selection');
  await act(async()=>harness.menu.onmessage('select-part'));
  assert.match(document.querySelector('[aria-label="Selection kind"]').textContent,/Pick parts/);
  await click(button('Dimensions'));
  await click(document.querySelector('[aria-label="Dimension scope"]'));
  assert.equal(harness.calls.findLast(([name])=>name==='popup_native_menu')[1].kind,'scope');
  await act(async()=>harness.menu.onmessage('scope-all'));
  assert.match(document.querySelector('[aria-label="Dimension scope"]').textContent,/All visible/);
  assert.ok(harness.viewer.some(([name,visible,scope])=>name==='dimensions'&&visible&&scope==='all'));
  const status=harness.calls.findLast(([name])=>name==='update_native_menu')[1].status;
  assert.equal(status.selection_mode,'part');
  assert.equal(status.dimension_scope,'all');
});
test('dimension overlays toggle without a Measure inspector',async()=>{
  assert.equal(document.getElementById('tab-measure'),null);
  assert.equal(document.getElementById('panel-measure'),null);
  await click(button('Dimensions'));
  assert.ok(harness.viewer.some(call=>call[0]==='dimensions'&&call[1]===true));
  assert.equal(document.getElementById('panel-model').hidden,false);
  await click(document.querySelector('[aria-label="Selection kind"]'));
  assert.equal(harness.calls.findLast(call=>call[0]==='popup_native_menu')[1].kind,'selection');
  await act(async()=>harness.menu.onmessage('select-edge'));
  assert.ok(harness.viewer.some(call=>call[0]==='selectionMode'&&call[1]==='edge'));
  await act(async()=>harness.view.onmessage({id:'dimension-request',workspace_id:'workspace-1',revision:'r1',command:'dimensions',arguments:{visible:false,scope:'pinned'}}));
  assert.deepEqual(harness.calls.find(([name,args])=>name==='complete_view'&&args.requestId==='dimension-request')[1],{
    token:'view-token',requestId:'dimension-request',reply:{data:{workspace_id:'workspace-1',revision:'r1',visible:false,scope:'pinned'}}
  });
  assert.ok(harness.viewer.some(call=>call[0]==='dimensions'&&call[1]===false&&call[2]==='pinned'));
});
test('motion controls retain explicit units, simulation setup, recording and interference access',async()=>{
  await openMotion();
  const motionPanel=document.getElementById('panel-motion');
  assert.equal(document.getElementById('tab-motion').getAttribute('aria-expanded'),'true');
  assert.equal(motionPanel.querySelector('.physics-workflow').hidden,true);
  assert.equal(motionPanel.getAttribute('aria-label'),'Move');
  assert.equal(motionPanel.querySelector('.motion-task-picker'),null);
  assert.equal(button('New requirement…'),undefined);
  assert.equal(motionPanel.querySelector('.recording-tools').open,false);
  assert.equal(document.getElementById('tab-recordings'),null);
  await chooseMotionTask('Physics');assert.ok(button('Run simulation'));
  assert.equal(motionPanel.querySelector('.physics-workflow').hidden,false);
  assert.equal(motionPanel.querySelector('.manual-motion').hidden,true);
  await chooseMotionTask('Move');
  assert.equal(button('Record motion'),undefined,'Recording starts in its own view');
  await openMotionSection('Record and replay');assert.ok(button('Record motion'));
  assert.ok(button('Refresh recordings'));
  await chooseMotionTask('Move');
  await openMotionSection('Check for overlapping parts');assert.ok(button('Check interference'));
  assert.ok(document.querySelector('[aria-label="Drag in canvas"]'));
  assert.equal(motionPanel.textContent.includes('Choose a position'),false);
  const slide=document.querySelector('[aria-label="slide"]');
  assert.match(slide.getAttribute('aria-valuetext'),/mm/);
  await setValue(slide,'120');await flush();
  assert.equal(document.querySelector('[aria-label="slide"]').getAttribute('aria-valuetext'),'120 mm');
  const request=harness.calls.find(call=>call[0]==='joint_coordinate')?.[1].request;
  assert.ok(request);assert.equal(request.coordinate,.12);assert.equal(request.unit,'m');assert.equal(request.expected_pose_version,0);
});
test('manual rotation uses degrees while joint commands retain radians',async()=>{
  harness.project.motion.joints=[{id:'hinge',child:'carriage',kind:'revolute',unit:'rad',coordinate:Math.PI/2,limits:[0,Math.PI],drivable:true}];
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await openMotion();
  const input=document.querySelector('[aria-label="hinge"]');
  assert.equal(input.getAttribute('aria-valuetext'),'90 °');
  assert.match(document.querySelector('.motion-control').textContent,/carriage/);
  await setValue(input,'45');await flush();
  const request=harness.calls.findLast(([name])=>name==='joint_coordinate')[1].request;
  assert.equal(request.unit,'rad');assert.ok(Math.abs(request.coordinate-Math.PI/4)<1e-9);
});
test('physics choices send material, contact and gravity values to the simulation',async()=>{
  harness.project.motion.joints[0].child='carriage';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  const invoke=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command!=='simulation')return invoke(command,args);
    harness.calls.push([command,args]);return structuredClone(harness.project.motion);
  };
  await openMotion();await chooseMotionTask('Physics');
  assert.equal(document.querySelector('.physics-custom').open,false);
  await setValue(document.getElementById('physics-material'),'7850','change');
  await setValue(document.getElementById('physics-grip'),'0.1','change');
  await setValue(document.getElementById('physics-bounce'),'0.2','change');
  await setValue(document.getElementById('physics-gravity'),'off','change');
  await click(button('Run simulation'));
  const definition=harness.calls.findLast(([name])=>name==='simulation')[1].request.command.definition;
  assert.deepEqual(definition.gravity_m_s2,[0,0,0]);
  assert.deepEqual(definition.bodies.carriage,{type:'dynamic',mass:{type:'density',density_kg_m3:7850},friction:0.1,restitution:0.2});
  assert.equal(definition.bodies.rail.type,'fixed');
  await setValue(document.getElementById('physics-gravity'),'custom','change');
  assert.equal(document.querySelector('.physics-custom').open,true);
});
test('Assistant remains visible while Explorer and tools share the left sidebar',async()=>{
  const assistant=document.querySelector('.assistant-panel');
  const composer=document.querySelector('.prompt-form');
  const model=document.getElementById('panel-model'),motion=document.getElementById('panel-motion');
  assert.equal(assistant.contains(model),false);assert.equal(assistant.contains(motion),false);
  assert.equal(composer.hidden,false);assert.equal(model.hidden,false);assert.equal(motion.hidden,true);
  assert.ok(model.closest('.workbench'));assert.ok(motion.closest('.workbench-body'));assert.equal(motion.closest('.canvas-stack'),null);
  await setValue(document.getElementById('model-prompt'),'Keep this idea for later');
  await openMotion();await chooseMotionTask('Physics');
  assert.equal(model.hidden,true);assert.equal(composer.hidden,false);
  assert.equal(motion.hidden,false);
  await click(document.querySelector('[aria-label="Close motion panel"]'));
  assert.equal(motion.hidden,true);assert.equal(document.activeElement.id,'tab-physics');
  await openMotion();
  assert.equal(document.querySelector('.physics-workflow').hidden,false,'Closing a panel retains its setup');
  await act(async()=>harness.select({revision:'r1',instance_id:'rail',entity_id:'part:0'}));
  assert.equal(model.hidden,true,'Selecting geometry keeps the active tool sidebar visible');
  assert.equal(document.getElementById('model-prompt').value,'Keep this idea for later');
  assert.equal(harness.calls.some(([name])=>name==='joint_coordinate'||name==='simulation'),false);
  assert.equal(harness.viewer.some(([name])=>name==='dispose'),false);
});
test('new motion test accepts free text and sends the user requirement to the agent',async()=>{
  await openMotion();await chooseMotionTask('Requirements');
  assert.match(document.querySelector('.test-empty').textContent,/No requirements yet/);
  assert.equal(document.querySelector('.motion-tests .panel-hint'),null);
  assert.equal(document.querySelector('.test-simulation-limits'),null);
  assert.equal(document.querySelector('#panel-motion .motion-state'),null);
  await click(button('New requirement…'));
  assert.equal(button('Prepare requirement').disabled,true);
  await setValue(document.getElementById('test-intent'),'The cam must lift the follower at least 8 mm without losing contact.');
  await click(button('Prepare requirement'));
  assert.equal(document.getElementById('panel-assistant').hidden,false);
  const prompt=harness.calls.findLast(([name])=>name==='agent_start')[1].prompt;
  assert.match(prompt,/saved CAD requirement/);
  assert.match(prompt,/The cam must lift the follower at least 8 mm without losing contact\./);
  assert.match(prompt,/do not invent acceptance criteria/);
  assert.match(prompt,/do not run them/);
  assert.equal(harness.calls.some(([name,args])=>name==='motion_tests'&&args.request.command.action==='start_run'),false);
});
test('free text motion tests expose actual criteria and simulation assumptions before running',async()=>{
  const definition={name:'Lever pushes shoe',intent:'Make sure the lever reaches the shoe and pushes it.',
    initial_coordinates:{slide:0,hinge:0},
    simulation:{gravity_m_s2:[0,0,-9.81],bodies:{lever:{type:'dynamic',mass:{type:'kilograms',mass_kg:.2},friction:.3,restitution:0},shoe:{type:'dynamic',mass:{type:'density',density_kg_m3:7800},friction:.4,restitution:.1},rail:{type:'fixed',friction:.5,restitution:0}}},
    actions:[{ticks:60,motors:[{joint_id:'slide',target:.01,unit:'m',max_force:2,stiffness:100,damping:10},{joint_id:'hinge',target:Math.PI/2,unit:'rad',max_force:.3,stiffness:5,damping:1}],loads:{shoe:{force_n:[-.5,0,0],torque_nm:[0,0,.1]}}},{ticks:120,motors:[],loads:{}}],
    checks:[{type:'push',pusher:'lever',receiver:'shoe',driver_joint:'hinge',direction:[1,0,0],minimum_mm:5,minimum_additional_mm:4,minimum_sampled_impulse_ns:.000001},{type:'coordinate',joint_id:'slide',target:.01,unit:'m',tolerance:.0005},{type:'coordinate',joint_id:'hinge',target:Math.PI/2,unit:'rad',tolerance:Math.PI/180},{type:'displacement',body:'shoe',direction:[0,-1,0],minimum_mm:2}]};
  const invoke=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>command==='motion_tests'?{catalog:'review-catalog',tests:[{id:'push',definition,outcome:null,stale:true,frame_count:0,duration_s:0}],accuracy:'Approximate proxy simulation',snapshot:null}:invoke(command,args);
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');
  const review=document.querySelector('.test-definition');assert.ok(review);
  const criteria=review.querySelector('.test-criteria').textContent;
  assert.match(criteria,/lever pushes shoe/);
  assert.match(criteria,/at least 5 mm.*world \+X/);
  assert.match(criteria,/at least 4 mm farther.*hinge motor disabled/);
  assert.match(criteria,/normal impulse of at least 0.000001 N s/);
  assert.match(criteria,/slide finishes at 10 mm, within ±0.5 mm/);
  assert.match(criteria,/hinge finishes at 90°, within ±1°/);
  assert.match(criteria,/shoe finishes at least 2 mm.*world −Y/);
  assert.ok(!review.querySelector('.test-criteria').closest('details'),'checks must be visible before Check requirement');
  assert.match(review.textContent,/0–0.5 s/);assert.match(review.textContent,/0.5–1.5 s/);
  assert.match(review.textContent,/Drive slide toward 10 mm, limited to 2 N/);
  assert.match(review.textContent,/Drive hinge toward 90°, limited to 0.3 N m/);
  assert.match(review.textContent,/world force \(-0.5, 0, 0\) N at its centre of mass/);
  assert.match(review.textContent,/Spring stiffness 100 N\/m, damping 10 N s\/m/);
  assert.match(review.textContent,/Spring stiffness 5 N m\/rad, damping 1 N m s\/rad/);
  assert.match(review.textContent,/Keep previous motor targets/);assert.match(review.textContent,/No external loads/);
  assert.match(review.textContent,/World gravity \(0, 0, -9.81\) m\/s²/);
  assert.match(review.textContent,/lever: 0.2 kg; friction 0.3, restitution 0/);
  assert.match(review.textContent,/shoe: density 7800 kg\/m³/);
  assert.match(review.textContent,/rail: fixed/);
  const technical=[...review.querySelectorAll('summary')].find(item=>item.textContent==='Technical definition');
  assert.equal(technical.parentElement.open,false);
  assert.equal(technical.parentElement.querySelector('pre'),null);
  await click([...review.querySelectorAll('summary')].find(item=>item.textContent==='Setup and steps'));
  await click(technical);await flush();
  assert.equal(technical.parentElement.open,true);
  assert.equal(JSON.parse(technical.parentElement.querySelector('pre').textContent).checks[0].minimum_additional_mm,4);
  assert.ok(button('Check requirement'));assert.ok(document.querySelector('[aria-label="Delete requirement Lever pushes shoe"]'));
  assert.match(document.querySelector('.test-status').textContent,/Ready to check/);
  await click(document.querySelector('[aria-label="Edit requirement Lever pushes shoe"]'));
  assert.equal(document.getElementById('test-intent').value,definition.intent);
  await setValue(document.getElementById('test-intent'),'Push the shoe at least 7 mm.');
  await click(button('Update checks'));
  const edit=harness.calls.findLast(([name])=>name==='agent_start')[1].prompt;
  assert.match(edit,/ID push/);assert.match(edit,/at least 7 mm/);assert.match(edit,/do not run them/);
});
test('saved motion test renders measured failures and reruns through the catalog CAS',async()=>{
  const invoke=harness.invoke.bind(harness);let calls=0;
  const definition={name:'Lever reaches shoe',intent:'Push the shoe by 5 mm',actions:[],checks:[{type:'displacement',body:'shoe',direction:[1,0,0],minimum_mm:5}]};
  const outcome={passed:false,measurements:[{label:'shoe: displacement',actual:2,expected:5,unit:'mm',passed:false}]};
  harness.invoke=async(command,args)=>{
    if(command==='motion_tests'){
      calls++;harness.calls.push([command,args]);
      const action=args.request.command;
      const snapshot=action.action==='replay'?{...harness.project.motion,pose_version:action.expected_pose_version+1,playback:{recording_id:'motion-test:push',name:definition.name,frame_index:action.frame_index,frame_count:121,time_s:action.frame_index/120,duration_s:1,source_mode:'simulate'}}:null;
      return {catalog:'catalog-2',tests:[{id:'push',definition,outcome,stale:false,frame_count:121,duration_s:1}],accuracy:'Approximate proxy contacts',snapshot};
    }
    return invoke(command,args);
  };
  // The background catalog refresh also notices tests created by external agents.
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');
  assert.match(document.querySelector('.motion-test').textContent,/Not met/);
  assert.match(document.querySelector('.test-intent').textContent,/Push the shoe by 5 mm/);
  const criterion=document.querySelector('.test-criterion.failed');
  assert.ok(criterion);
  assert.match(criterion.textContent,/shoe finishes at least 5 mm.*world \+X/);
  assert.match(criterion.textContent,/Not met/);
  assert.match(criterion.querySelector('.test-measurements').textContent,/Measured 2 mm · minimum 5 mm/);
  assert.match(document.querySelector('.test-definition').textContent,/Results against requirements/);
  assert.match(document.querySelector('.test-definition').textContent,/0 of 1 requirements met/);
  await click(button('Check again'));
  const request=harness.calls.findLast(([name,args])=>name==='motion_tests'&&args.request.command.action==='start_run')[1].request;
  assert.deepEqual(request,{workspace_id:'workspace-1',revision:'r1',expected_catalog:'catalog-2',command:{action:'start_run',id:'push'}});
  assert.ok(calls>=2);
  await click(button('Inspect replay'));
  assert.ok(document.querySelector('[aria-label="Requirement replay time"]'));
  assert.equal(document.querySelector('[aria-label="Recorded frame"]'),null,'test traces must use the test replay endpoint');
  assert.ok(button('Play replay'));
  assert.ok(button('Leave playback'));
  const replay=harness.calls.findLast(([name,args])=>name==='motion_tests'&&args.request.command.action==='replay')[1].request;
  assert.equal(replay.command.id,'push');assert.equal(replay.command.frame_index,0);assert.equal(replay.command.expected_pose_version,0);
});

test('finishing a recording refreshes the document edited state for portable Save',async()=>{
  harness.project.dirty=false;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(document.querySelector('.document-edited'),null);
  const invoke=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command!=='playback')return invoke(command,args);
    harness.calls.push([command,args]);
    const project=harness.project;
    if(args.request.command.action==='start_recording')project.motion.recording={name:'Motion recording',frame_count:1,duration_s:0,capacity:3600,capacity_reached:false};
    if(args.request.command.action==='finish_recording'){
      delete project.motion.recording;project.dirty=true;
      project.recordings=[{id:'recording',name:'Motion recording',revision:'r1',source_mode:'manipulate',frame_count:1,duration_s:0,capacity_reached:false}];
    }
    project.motion.pose_version++;
    return {snapshot:structuredClone(project.motion),recordings:project.recordings,frame_times_s:null};
  };
  await openMotion();await openMotionSection('Record and replay');await click(button('Record motion'));await click(button('Finish recording'));
  assert.equal(document.querySelector('.document-edited').textContent,'Edited');
  assert.match(document.getElementById('panel-motion').textContent,/Save your project to take completed recordings with you/);
});

test('recording notifications from another actor refresh the focused document',async()=>{
  harness.project.dirty=false;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(document.querySelector('.document-edited'),null);
  harness.project.dirty=true;
  harness.project.recordings=[{id:'external',name:'External motion',revision:'r1',source_mode:'manipulate',frame_count:2,duration_s:1,capacity_reached:false}];
  await act(async()=>{harness.activity.onmessage({workspace_id:'workspace-1',kind:'recordings_changed'});await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(document.querySelector('.document-edited').textContent,'Edited');
  await openMotion();
  assert.match(document.getElementById('panel-motion').textContent,/External motion/);
});

test('simulation load editor sends SI vectors with pose CAS and preserves other body loads',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command!=='simulation')return original(command,args);
    harness.calls.push([command,args]);
    const current=harness.project.motion,request=args.request;
    assert.equal(request.workspace_id,current.workspace_id);
    assert.equal(request.revision,current.revision);
    assert.equal(request.expected_pose_version,current.pose_version);
    if(request.command.action==='start'){
      current.mode='simulate';current.simulation={timestep_s:1/120,accuracy:'fixture',mass_kg:{carriage:.1,rail:1},external_loads:{rail:{force_n:[0,.1,0],torque_nm:[0,0,0]}},previous:{tick:0,time_s:0,poses:{}},current:{tick:0,time_s:0,poses:{}}};
    }else if(request.command.action==='loads')current.simulation.external_loads=request.command.loads;
    current.pose_version++;return structuredClone(current);
  };
  await openMotion();await chooseMotionTask('Physics');await click(button('Run simulation'));await click(button('Pause simulation'));
  await click([...document.querySelectorAll('summary')].find(element=>element.textContent==='Force and torque'));
  await setValue(document.querySelector('[aria-label="Force X (N)"]'),'.2');
  await setValue(document.querySelector('[aria-label="Torque Z (N m)"]'),'.004');
  await click(button('Apply load'));
  const command=harness.calls.filter(([name])=>name==='simulation').at(-1)[1].request.command;
  assert.deepEqual(command,{action:'loads',loads:{carriage:{force_n:[.2,0,0],torque_nm:[0,0,.004]},rail:{force_n:[0,.1,0],torque_nm:[0,0,0]}}});
  await click(button('Clear body load'));
  assert.deepEqual(harness.project.motion.simulation.external_loads,{rail:{force_n:[0,.1,0],torque_nm:[0,0,0]}});
  await click(button('Clear all loads'));
  assert.deepEqual(harness.project.motion.simulation.external_loads,{});
  await setValue(document.querySelector('[aria-label="Force X (N)"]'),'');
  assert.equal(button('Apply load').disabled,true);
});
test('native menus dispatch advanced tools, document actions and independent panels',async()=>{
  assert.equal(document.querySelector('.app-header select'),null,'Document title must use a native menu rather than an HTML dropdown');
  assert.equal(document.querySelector('[aria-label="Switch workspace"]'),null);
  assert.equal(document.querySelector('.document-title')?.tagName,'DIV');
  assert.equal(document.querySelector('.app-header [aria-label="Stop assistant"]'),null,'Assistant Stop belongs in the composer, not the title bar');
  assert.equal(document.querySelector('.app-header .status'),null,'Agent status does not belong in the title bar');
  assert.equal(document.querySelector('.document-caption')?.textContent,'Model');
  assert.equal(document.querySelector('.document-name')?.textContent,'Rail assembly');
  const menu=harness.calls.findLast(([name])=>name==='update_native_menu');
  assert.deepEqual(menu[1].status.workspaces,[{id:'workspace-1',name:'Rail assembly',selected:true,running:false}]);
  const before=harness.calls.length;
  await act(async()=>harness.menu.onmessage('workspace:unknown'));
  assert.equal(harness.calls.slice(before).some(([name])=>name==='switch_project'),false,'Stale native menu entries must not open unknown workspaces');
  assert.ok(!document.querySelector('[aria-label="Export model"]'),'Export is available from the native File menu');
  assert.equal(document.querySelector('.native-view'),null,'Canvas View popup duplicates camera controls and the menubar');
  await act(async()=>harness.menu.onmessage('advanced'));
  assert.ok(harness.viewer.some(call=>call[0]==='advanced'&&call[1]===true));
  await act(async()=>harness.menu.onmessage('save'));
  assert.ok(harness.calls.some(([name,args])=>name==='save_project'&&args.workspace==='workspace-1'));
  await act(async()=>harness.menu.onmessage('dimensions'));
  assert.ok(harness.calls.some(([name,args])=>name==='update_native_menu'&&args.status.dimensions===true));
  assert.equal(document.getElementById('panel-model').hidden,false);
  await click(document.getElementById('tab-model'));
  assert.equal(document.getElementById('panel-model').hidden,true);
  await act(async()=>harness.menu.onmessage('panel-motion'));
  assert.equal(document.getElementById('panel-motion').hidden,false);
  assert.equal(document.getElementById('panel-assistant').hidden,false);
  const panels=harness.calls.findLast(([name])=>name==='update_native_menu')[1].status;
  assert.equal(panels.model_browser_open,false);assert.equal(panels.motion_panel_open,true);
  assert.equal(document.querySelector('.file-menu'),null);
});
test('settings and Assistant composer retain agent configuration and sending',async()=>{
  await act(async()=>harness.menu.onmessage('settings'));assert.equal(document.querySelector('dialog').open,true);
  await setValue(document.querySelector('.provider-settings select'),'anthropic_api','change');
  assert.equal(document.querySelector('.provider-settings input').value,'test-model');
  await click(document.querySelector('[aria-label="Close settings"]'));assert.equal(document.querySelector('dialog').open,false);
  await click(document.getElementById('tab-assistant'));
  const prompt=document.getElementById('model-prompt');await act(async()=>prompt.focus());
  assert.equal(document.getElementById('panel-assistant').hidden,false);
  await setValue(prompt,'Make a mounting bracket');
  await act(async()=>document.querySelector('.prompt-form').dispatchEvent(new dom.window.Event('submit',{bubbles:true,cancelable:true})));await flush();
  assert.ok(harness.calls.some(([name,args])=>name==='agent_start'&&args.workspace==='workspace-1'&&args.prompt==='Make a mounting bracket'));
});

const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
const agentTurn=(id,workspace_id,state='running')=>({id,workspace_id,state,text:'',activity:'Connecting to the model',error:null,artifacts:[]});
async function submitPrompt(text){if(document.getElementById('panel-assistant').hidden)await click(document.getElementById('tab-assistant'));await setValue(document.getElementById('model-prompt'),text);await act(async()=>{document.querySelector('.prompt-form').dispatchEvent(new dom.window.Event('submit',{bubbles:true,cancelable:true}));});}
const status=()=>document.querySelector('[data-agent-chrome-status]')?.textContent??'';
const stopAssistant=()=>document.querySelector('[aria-label="Stop assistant"]');

const testRun=(workspace,state='queued')=>({id:`run-${workspace}`,workspace_id:workspace,revision:'r1',test_id:'push',actor:'local-user',state,error:null});
const motionTestReply=(run=null)=>({catalog:'test-catalog',tests:[{id:'push',definition:{name:'Push check',intent:'Push at least 5 mm',actions:[],checks:[]},outcome:{passed:true,measurements:[{label:'Previous displacement',actual:6,expected:5,unit:'mm',passed:true}]},stale:false,frame_count:121,duration_s:1}],accuracy:'Fixture simulation',snapshot:null,run});
test('motion test Stop during pending start stays bound to the original document while another test starts',async()=>{
  const start=deferred(),invoke=harness.invoke.bind(harness),runs={};
  const other=fixture();other.workspace_id='workspace-2';other.name='Second document';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad')return structuredClone(other);
    if(command!=='motion_tests')return invoke(command,args);
    harness.calls.push([command,args]);const request=args.request;
    if(request.command.action==='start_run'){
      if(request.workspace_id==='workspace-1')return start.promise;
      runs[request.workspace_id]=testRun(request.workspace_id);
    }
    if(request.command.action==='cancel_run')runs[request.workspace_id]=testRun(request.workspace_id,'cancelled');
    return motionTestReply(runs[request.workspace_id]??null);
  };
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');
  await click(button('Check again'));
  assert.match(document.querySelector('.test-run-status').textContent,/Starting check/);
  await click(button('Stop check'));
  assert.match(document.querySelector('.test-run-status').textContent,/Stopping check/);
  assert.equal(harness.calls.some(([name,args])=>name==='motion_tests'&&args.request.command.action==='cancel_run'),false);
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,20));});
  await openMotion();await chooseMotionTask('Requirements');await click(button('Check again'));
  assert.match(document.querySelector('.test-run-status').textContent,/Check queued/);
  runs['workspace-1']=testRun('workspace-1');
  await act(async()=>{start.resolve(motionTestReply(runs['workspace-1']));await new Promise(resolve=>setTimeout(resolve,15));});
  const stops=harness.calls.filter(([name,args])=>name==='motion_tests'&&args.request.command.action==='cancel_run').map(([,args])=>args.request);
  assert.deepEqual(stops,[{workspace_id:'workspace-1',revision:'r1',expected_catalog:null,command:{action:'cancel_run',run_id:'run-workspace-1'}}]);
  assert.match(document.querySelector('.test-run-status').textContent,/Check queued/);
  assert.equal(button('Stop check').disabled,false);
  assert.equal(document.querySelector('.document-name').textContent,'Second document');
});
test('active motion test Stop remains available and previous measurements survive cancellation',async()=>{
  const invoke=harness.invoke.bind(harness);let run=null;
  harness.invoke=async(command,args)=>{
    if(command!=='motion_tests')return invoke(command,args);
    harness.calls.push([command,args]);
    if(args.request.command.action==='start_run')run=testRun('workspace-1','running');
    if(args.request.command.action==='cancel_run')run={...run,state:'cancelling'};
    return motionTestReply(run);
  };
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');await click(button('Check again'));
  assert.match(document.querySelector('.test-run-status').textContent,/Checking requirement/);
  assert.equal(button('Check again').disabled,true);
  assert.match(document.querySelector('.test-measurements').textContent,/Previous displacement/);
  await click(button('Stop check'));
  assert.equal(button('Stop check').disabled,true);
  assert.match(document.querySelector('.test-run-status').textContent,/Stopping check/);
  run={...run,state:'cancelled'};await flush(350);
  assert.equal(button('Stop check'),undefined);assert.equal(button('Check again').disabled,false);
  assert.match(document.querySelector('.motion-tests').textContent,/Check stopped. Previous results preserved/);
  assert.match(document.querySelector('.test-measurements').textContent,/Measured 6 mm/);
});
test('slow motion-test status reads do not pile up or suppress the terminal response',async()=>{
  const invoke=harness.invoke.bind(harness),read=deferred();let run=null,hold=false,reads=0;
  harness.invoke=async(command,args)=>{
    if(command!=='motion_tests')return invoke(command,args);
    if(args.request.command.action==='start_run'){run=testRun('workspace-1','running');hold=true;return motionTestReply(run);}
    if(hold&&args.request.command.action==='list'){reads++;return read.promise;}
    return motionTestReply(run);
  };
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');await click(button('Check again'));
  await flush(700);assert.equal(reads,1);
  assert.ok(button('Stop check'));
  hold=false;run={...run,state:'completed'};
  await act(async()=>{read.resolve(motionTestReply(run));await new Promise(resolve=>setTimeout(resolve,15));});
  assert.equal(button('Stop check'),undefined);
  assert.equal(button('Check again').disabled,false);
  assert.match(document.querySelector('.test-status').textContent,/Met/);
  assert.equal(document.querySelector('.test-run-status'),null);
});

test('agent submission acknowledges before IPC replies and queues one Stop for its accepted turn',async()=>{
  const start=deferred(),invoke=harness.invoke.bind(harness);let turn=null;
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return {messages:[],turn};
    if(command==='agent_start'){harness.calls.push([command,args]);return start.promise;}
    if(command==='agent_stop'){harness.calls.push([command,args]);turn={...turn,state:'cancelled'};return turn;}
    return invoke(command,args);
  };
  await submitPrompt('Make a small bracket');
  assert.equal(status(),'Starting request…');assert.ok(stopAssistant());
  await act(async()=>document.querySelector('.prompt-form').dispatchEvent(new dom.window.Event('submit',{bubbles:true,cancelable:true})));
  assert.equal(harness.calls.filter(([name])=>name==='agent_start').length,1);
  await click(stopAssistant());assert.equal(status(),'Stopping…');assert.equal(stopAssistant().disabled,true);
  assert.equal(harness.calls.some(([name])=>name==='agent_stop'),false,'No turn ID exists before admission replies');
  turn=agentTurn('accepted-a','workspace-1');
  await act(async()=>{start.resolve(turn);await new Promise(resolve=>setTimeout(resolve,10));});
  assert.deepEqual(harness.calls.filter(([name])=>name==='agent_stop').map(([,args])=>args),[{turnId:'accepted-a'}]);
  assert.equal(status(),'');assert.equal(document.getElementById('model-prompt').value,'Make a small bracket');
});

test('a queued Stop and late start reply stay bound to the original workspace',async()=>{
  const starts={'workspace-1':deferred(),'workspace-2':deferred()},turns={},invoke=harness.invoke.bind(harness);
  const other=fixture();other.workspace_id='workspace-2';other.name='Second document';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad')return structuredClone(other);
    if(command==='agent_start'){harness.calls.push([command,args]);return starts[args.workspace].promise;}
    if(command==='agent_conversation')return {messages:[],turn:turns[args.workspace]||null};
    if(command==='agent_stop'){harness.calls.push([command,args]);turns['workspace-1']=agentTurn('first','workspace-1','cancelled');return turns['workspace-1'];}
    return invoke(command,args);
  };
  await submitPrompt('First idea');await click(stopAssistant());
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});
  await submitPrompt('Second idea');assert.equal(status(),'Starting request…');
  turns['workspace-1']=agentTurn('first','workspace-1');
  await act(async()=>{starts['workspace-1'].resolve(turns['workspace-1']);await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(status(),'Starting request…','Completing the background request must not clear the focused start');
  assert.equal(document.getElementById('model-prompt').value,'Second idea');
  assert.deepEqual(harness.calls.filter(([name])=>name==='agent_start').map(([,args])=>args.workspace),['workspace-1','workspace-2']);
  assert.deepEqual(harness.calls.filter(([name])=>name==='agent_stop').map(([,args])=>args.turnId),['first']);
  turns['workspace-2']=agentTurn('second','workspace-2','succeeded');
  await act(async()=>{starts['workspace-2'].resolve(turns['workspace-2']);await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(status(),'');assert.equal(document.querySelector('.document-name').textContent,'Second document');
});

test('late start and conversation replies cannot revive a completed agent turn',async()=>{
  const start=deferred(),staleConversation=deferred(),invoke=harness.invoke.bind(harness);let turn=null,holdConversation=true;
  harness.invoke=async(command,args)=>{
    if(command==='agent_start'){harness.calls.push([command,args]);return start.promise;}
    if(command==='agent_conversation'){if(holdConversation){holdConversation=false;return staleConversation.promise;}return {messages:[],turn};}
    return invoke(command,args);
  };
  await submitPrompt('A quick change');turn=agentTurn('quick','workspace-1');
  await act(async()=>harness.agent.onmessage({kind:'turn_started',workspace_id:'workspace-1',turn_id:'quick',turn}));
  turn={...turn,state:'succeeded',text:'Done'};
  await act(async()=>{harness.agent.onmessage({kind:'turn_finished',workspace_id:'workspace-1',turn_id:'quick',turn});await new Promise(resolve=>setTimeout(resolve,5));});
  await act(async()=>{staleConversation.resolve({messages:[],turn:agentTurn('quick','workspace-1')});start.resolve(agentTurn('quick','workspace-1'));await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(status(),'');assert.equal(stopAssistant(),null);assert.equal(document.querySelector('.streaming'),null);
});

test('a late cancelling reply cannot replace a terminal event or a newer agent turn',async()=>{
  const stop=deferred(),invoke=harness.invoke.bind(harness);let turn=agentTurn('old','workspace-1');
  harness.invoke=async(command,args)=>{
    if(command==='agent_start')return turn;
    if(command==='agent_conversation')return {messages:[],turn};
    if(command==='agent_stop'){harness.calls.push([command,args]);return stop.promise;}
    return invoke(command,args);
  };
  await submitPrompt('Original request');
  // Do not await a deliberately held native Stop reply.
  await act(async()=>{stopAssistant().click();});assert.equal(status(),'Stopping…');
  turn={...turn,state:'cancelled'};
  await act(async()=>{harness.agent.onmessage({kind:'turn_finished',workspace_id:'workspace-1',turn_id:'old',turn});await new Promise(resolve=>setTimeout(resolve,5));});
  turn=agentTurn('new','workspace-1');await submitPrompt('New request');
  await act(async()=>{stop.resolve(agentTurn('old','workspace-1','cancelling'));await new Promise(resolve=>setTimeout(resolve,5));});
  assert.equal(status(),'Connecting to the model');assert.equal(stopAssistant().disabled,false);
});

test('a cancelled unaccepted request does not open settings for a late configuration error',async()=>{
  const start=deferred(),invoke=harness.invoke.bind(harness);
  harness.invoke=(command,args)=>command==='agent_start'?start.promise:invoke(command,args);
  await submitPrompt('Try this idea');await click(stopAssistant());
  await act(async()=>{start.reject({code:'LLM_NOT_CONFIGURED',message:'Missing key'});await new Promise(resolve=>setTimeout(resolve,5));});
  assert.equal(document.querySelector('dialog').open,false);assert.equal(document.querySelector('[role="alert"]'),null);assert.equal(status(),'');
});

test('Stop still reaches an accepted turn while its conversation refresh is pending',async()=>{
  const conversation=deferred(),invoke=harness.invoke.bind(harness);let turn=agentTurn('accepted','workspace-1');
  harness.invoke=async(command,args)=>{
    if(command==='agent_start')return turn;
    if(command==='agent_conversation')return conversation.promise;
    if(command==='agent_stop'){harness.calls.push([command,args]);turn={...turn,state:'cancelled'};return turn;}
    return invoke(command,args);
  };
  await submitPrompt('A slow conversation refresh');await click(stopAssistant());
  assert.ok(harness.calls.some(([name,args])=>name==='agent_stop'&&args.turnId==='accepted'));
  await act(async()=>{conversation.resolve({messages:[],turn:agentTurn('accepted','workspace-1')});await new Promise(resolve=>setTimeout(resolve,5));});
  assert.equal(status(),'');assert.equal(stopAssistant(),null);
});

test('a failed Stop preserves streamed text and permits retry without reviving an old snapshot',async()=>{
  const stop=deferred(),invoke=harness.invoke.bind(harness);const turn=agentTurn('retry','workspace-1');
  harness.invoke=async(command,args)=>{
    if(command==='agent_start')return turn;
    if(command==='agent_conversation')return {messages:[],turn};
    if(command==='agent_stop')return stop.promise;
    return invoke(command,args);
  };
  await submitPrompt('Stream while stopping');
  await act(async()=>harness.agent.onmessage({kind:'text_delta',workspace_id:'workspace-1',turn_id:'retry',delta:'Some text'}));
  await act(async()=>stopAssistant().click());
  await act(async()=>harness.agent.onmessage({kind:'text_delta',workspace_id:'workspace-1',turn_id:'retry',delta:' continues'}));
  await act(async()=>{stop.reject({message:'Try stopping again'});await new Promise(resolve=>setTimeout(resolve,5));});
  assert.equal(stopAssistant().disabled,false);assert.equal(status(),'Connecting to the model');
  assert.match(document.querySelector('.streaming').textContent,/Some text continues/);
});

test('example entry points are absent from Assistant, empty models, movement and menu dispatch',async()=>{
  assert.equal(document.querySelector('.starter-disclosure'),null);
  assert.equal(button('Rail & carriage example'),undefined);
  harness.project.model.parts=[];harness.project.parameters={};harness.project.motion.joints=[];
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await openMotion();
  assert.ok(button('Set up movement'));
  assert.equal(button('Try a moving example'),undefined);
  assert.equal(button('Try an example'),undefined);
  assert.doesNotMatch(document.querySelector('.empty').textContent,/example/i);
  await act(async()=>harness.menu.onmessage('example-channel'));
  assert.equal(harness.calls.some(([name])=>name==='example'),false);
});

test('native Undo edits focused text without undoing the committed CAD model',async()=>{
  const edits=[];document.execCommand=command=>{edits.push(command);return true;};
  await click(button('Assistant'));
  await act(async()=>document.getElementById('model-prompt').focus());
  await act(async()=>harness.menu.onmessage('undo'));
  assert.deepEqual(edits,['undo']);assert.ok(!harness.calls.some(([name])=>name==='undo'));
  assert.ok(harness.calls.some(([name,args])=>name==='update_native_menu'&&args.status.editing));
  delete document.execCommand;
});

test('native Model commands share inspector visibility and publish current menu state',async()=>{
  assert.equal(button('File'),undefined);
  await click(button('rail'));
  await act(async()=>harness.menu.onmessage('isolate'));
  assert.ok(harness.viewer.some(([name,id,visible])=>name==='visible'&&id==='carriage'&&visible===false));
  assert.ok(document.querySelector('[aria-label="Show carriage"]'));
  let menu=harness.calls.filter(([name])=>name==='update_native_menu').at(-1)[1].status;
  assert.equal(menu.has_selection,true);assert.equal(menu.has_hidden_parts,true);
  await act(async()=>harness.menu.onmessage('show-all'));
  assert.ok(document.querySelector('[aria-label="Hide carriage"]'));
  assert.ok(harness.viewer.some(([name,id,visible])=>name==='visible'&&id==='carriage'&&visible===true));
  await act(async()=>harness.menu.onmessage('panel-motion'));
  menu=harness.calls.filter(([name])=>name==='update_native_menu').at(-1)[1].status;
  assert.equal(menu.inspector,'motion');assert.equal(menu.has_hidden_parts,false);
  assert.equal(document.getElementById('panel-motion').hidden,false);
});

test('simulation availability follows the native capability reason for closed graphs',async()=>{
  harness.project.motion.constraints={position_error_m:0,axis_error_rad:0,path_certified:true};
  harness.project.motion.simulation_unavailable_reason=null;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await openMotion();
  await chooseMotionTask('Physics');
  assert.equal(button('Run simulation').disabled,false,'Supported physical closure is explicitly available');
  harness.project.motion.simulation_unavailable_reason='Simulation for gear couplings is not available yet';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(button('Run simulation').disabled,true);
  assert.match(document.getElementById('panel-motion').textContent,/Simulation for gear couplings is not available yet/);
  delete harness.project.motion.simulation_unavailable_reason;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(button('Run simulation').disabled,true,'Legacy snapshots do not silently enable an unknown closure mapping');
});

for(const recover of [true,false])test(`display generation refresh ${recover?'recovers a stale load':'bounds repeated load failures'}`,async()=>{
  const first=fixture();first.workspace_id='workspace-2';first.revision='r2';first.display_generation='display-a';first.motion={...first.motion,workspace_id:first.workspace_id,revision:first.revision};
  const fresh={...first,display_generation:'display-b'};
  const invoke=harness.invoke.bind(harness);let loads=0;
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad'){harness.calls.push([command,args]);return structuredClone(first);}
    if(command==='project'&&args.workspace==='workspace-2'){harness.calls.push([command,args]);return structuredClone(fresh);}
    return invoke(command,args);
  };
  harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace==='workspace-2'&&(++loads===1||!recover))throw new Error('Expired display descriptor');};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,30));});await flush();
  assert.equal(loads,2,'The fresh generation must reload even when the design revision is unchanged');
  assert.equal(harness.calls.filter(([name,args])=>name==='project'&&args.workspace==='workspace-2').length,1,'Only one automatic refresh may follow consecutive failures');
  if(recover)assert.equal(document.querySelector('[role="alert"]'),null);
  else assert.match(document.querySelector('[role="alert"]').textContent,/Expired display descriptor/);
});

test('a cache lost after metadata was read exposes Restore display without executing a build',async()=>{
  const first=fixture();first.workspace_id='workspace-2';first.revision='r2';first.display_generation='display-a';first.motion={...first.motion,workspace_id:first.workspace_id,revision:first.revision};
  const missing={...first,display_error:{code:'DISPLAY_CACHE_MISSING',message:'Display data is missing. Restore display to regenerate it.'}};
  const invoke=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad'){harness.calls.push([command,args]);return structuredClone(first);}
    if(command==='project'&&args.workspace==='workspace-2'){harness.calls.push([command,args]);return structuredClone(missing);}
    return invoke(command,args);
  };
  harness.load=async(model,_revision,_artifact,workspace)=>{if(workspace==='workspace-2'&&model.parts.length)throw new Error('Missing display file');};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,30));});await flush();
  assert.ok(button('Restore display'));
  assert.equal(harness.calls.some(([name])=>name==='rebuild'||name==='repair_display'),false,'Refreshing a read must not execute source automatically');
  assert.equal(harness.calls.filter(([name,args])=>name==='project'&&args.workspace==='workspace-2').length,1);
});

test('same-revision repair preserves inspector visibility and selection',async()=>{
  harness.project.display_generation='display-a';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await click(document.querySelector('[aria-label="Hide carriage"]'));await click(button('rail'));await openMotion();
  const loads=harness.viewer.filter(([name])=>name==='load').length;
  harness.project.display_generation='display-b';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.ok(document.querySelector('[aria-label="Show carriage"]'));
  assert.equal(document.querySelector('.part-select[aria-pressed="true"]').textContent.trim(),'rail');
  assert.equal(document.getElementById('panel-motion').hidden,false);
  assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads+1);
});

test('missing cached files retain an already presented scene and its inspector state',async()=>{
  await click(document.querySelector('[aria-label="Hide carriage"]'));await click(button('rail'));
  const loads=harness.viewer.filter(([name])=>name==='load').length;
  harness.project.display_error={code:'DISPLAY_CACHE_MISSING',message:'Restore the cached files'};
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.ok(button('Restore display'));assert.ok(document.querySelector('[aria-label="Show carriage"]'));
  assert.equal(document.querySelector('.part-select[aria-pressed="true"]').textContent.trim(),'rail');
  assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads,'Available verified geometry should stay displayed');
  assert.equal(harness.viewport.hasScene('workspace-1','r1'),true);
});

test('returning to a retained scene cancels an intervening load even when cache files are missing',async()=>{
  const hold=deferred(),entered=deferred(),invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.name='Second workspace';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad')return structuredClone(other);
    if(command==='switch_project')return {...structuredClone(harness.project),display_error:{code:'DISPLAY_CACHE_MISSING',message:'Restore the cached files'}};
    if(command==='workspaces')return [{workspace_id:'workspace-1',name:'Rail assembly',running:0},{workspace_id:'workspace-2',name:'Second workspace',running:0}];
    return invoke(command,args);
  };
  harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace==='workspace-2'){entered.resolve();await hold.promise;}};
  await click(document.querySelector('[aria-label="Hide carriage"]'));await click(button('rail'));
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await entered.promise;await flush();
  assert.ok(document.querySelector('[aria-label="Hide carriage"]'),'Changing workspace resets its UI visibility intent');assert.equal(document.querySelector('.part-select[aria-pressed="true"]'),null);
  await act(async()=>harness.select({revision:'r1',instance_id:'rail',entity_id:'face:0'}));assert.equal(document.querySelector('.part-select[aria-pressed="true"]'),null,'Picks from the previous workspace cannot select the new document');
  await act(async()=>harness.view.onmessage({id:'pending-capture',workspace_id:'workspace-2',revision:'r1',command:'capture',arguments:{}}));
  assert.equal(harness.captureCalls||0,0,'A previous workspace scene must never be captured under the incoming workspace');assert.equal(harness.calls.find(([name,args])=>name==='complete_view'&&args.requestId==='pending-capture')[1].reply.error.code,'VIEW_NOT_READY');
  await act(async()=>harness.annotation({kind:'distance',label:'Old workspace dimension',value:99,part:'rail',start:[0,0,0],end:[99,0,0]}));assert.equal(document.getElementById('panel-model').hidden,false);assert.equal(document.body.textContent.includes('Old workspace dimension'),false);
  await act(async()=>{harness.menu.onmessage('workspace:workspace-1');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.ok(button('Restore display'));assert.equal(harness.viewport.hasScene('workspace-1','r1'),true);assert.ok(document.querySelector('[aria-label="Show carriage"]'),'The inspector must reflect the retained scene');
  await act(async()=>{hold.resolve();await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(harness.viewport.hasScene('workspace-1','r1'),true);assert.equal(harness.viewport.hasScene('workspace-2','r1'),false);
});

test('visibility chosen during a new workspace load applies only to the incoming scene',async()=>{
  const hold=deferred(),entered=deferred(),invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=(command,args)=>command==='new_scratchpad'?Promise.resolve(structuredClone(other)):invoke(command,args);
  harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace==='workspace-2'){entered.resolve();await hold.promise;}};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await entered.promise;
  await click(document.querySelector('[aria-label="Hide carriage"]'));
  assert.equal(harness.viewport.hidden.has('carriage'),false,'The previous scene must not receive incoming workspace visibility');
  await act(async()=>{hold.resolve();await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(harness.viewport.hasScene('workspace-2','r1'),true);assert.equal(harness.viewport.hidden.has('carriage'),true);assert.ok(document.querySelector('[aria-label="Show carriage"]'));
});

test('Retry view reloads the same display generation without executing geometry',async()=>{
  const invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.display_generation='same-display';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{if(command==='new_scratchpad'||command==='project'&&args.workspace===other.workspace_id){harness.calls.push([command,args]);return structuredClone(other);}return invoke(command,args);};
  let attempts=0;harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace===other.workspace_id&&++attempts===1)throw new Error('Renderer setup failed');};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await flush();
  assert.match(document.querySelector('.view-recovery [role="alert"]').textContent,/Renderer setup failed/);
  await click(button('Retry view'));
  assert.equal(attempts,2);
  assert.equal(document.querySelector('.view-recovery'),null);
  assert.equal(document.querySelector('.viewport').getAttribute('aria-busy'),'false');
  assert.equal(harness.calls.some(([name])=>['rebuild','repair_display','parameter','example'].includes(name)),false);
});

for(const hide of [false,true])test(`incoming scene preserves queued camera and ${hide?'clears hidden':'retains visible'} selection`,async()=>{
  const hold=deferred(),entered=deferred(),invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{if(command==='new_scratchpad'){harness.calls.push([command,args]);return structuredClone(other);}return invoke(command,args);};
  harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace===other.workspace_id){entered.resolve();await hold.promise;}};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await entered.promise;
  assert.equal(document.querySelector('.viewport').getAttribute('aria-busy'),'true');
  assert.match(document.querySelector('.view-recovery').textContent,/Loading model/);
  const calls=harness.viewer.length;
  await click(button('rail'));await act(async()=>harness.menu.onmessage('front'));
  assert.equal(harness.viewer.slice(calls).some(([name])=>name==='view'||name==='select'),false,'Pending commands must not affect the prior scene');
  if(hide)await click(document.querySelector('[aria-label="Hide rail"]'));
  await act(async()=>hold.resolve());await flush();
  assert.equal(harness.viewport.selection?.instance_id,hide?undefined:'rail');
  assert.equal(document.querySelector('.part-select[aria-pressed="true"]')?.textContent.trim(),hide?undefined:'rail');
  assert.ok(harness.viewer.slice(calls).some(([name,view])=>name==='view'&&view==='front'));
  assert.equal(document.querySelector('.view-recovery'),null);
});

test('a superseded scene failure cannot overwrite the current ready view',async()=>{
  const hold=deferred(),entered=deferred(),invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.motion={...other.motion,workspace_id:other.workspace_id};
  let opens=0;harness.invoke=async(command,args)=>{if(command==='new_scratchpad'){harness.calls.push([command,args]);return structuredClone(opens++?harness.project:other);}return invoke(command,args);};
  harness.load=async(_model,_revision,_artifact,workspace)=>{if(workspace===other.workspace_id){entered.resolve();await hold.promise;}};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await entered.promise;
  await act(async()=>harness.menu.onmessage('front'));
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});
  await act(async()=>hold.reject(new Error('Superseded renderer failure')));await flush();
  assert.equal(document.querySelector('.view-recovery'),null);
  assert.equal(document.querySelector('.viewport').getAttribute('aria-busy'),'false');
  assert.equal(harness.viewer.some(([name,view])=>name==='view'&&view==='front'),false,'A camera command must stay scoped to its document');
});

test('graphics loss suspends captures and restores the current view without executing geometry',async()=>{
  await click(button('rail'));await click(document.querySelector('[aria-label="Hide carriage"]'));
  const loads=harness.viewer.filter(([name])=>name==='load').length;
  await act(async()=>harness.graphics('lost'));
  assert.match(document.querySelector('.view-recovery').textContent,/Waiting for graphics/);
  assert.equal(button('Retry view'),undefined);assert.equal(document.querySelector('.viewport').getAttribute('aria-busy'),'true');
  await act(async()=>harness.menu.onmessage('front'));
  await act(async()=>harness.view.onmessage({id:'lost-capture',workspace_id:'workspace-1',revision:'r1',command:'capture',arguments:{}}));
  assert.equal(harness.calls.findLast(([name])=>name==='complete_view')[1].reply.error.code,'VIEW_NOT_READY');
  assert.equal(harness.captureCalls||0,0);assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads);
  await act(async()=>harness.menu.onmessage('save'));await flush();
  assert.ok(harness.calls.some(([name])=>name==='save_project'));
  await act(async()=>harness.graphics('restored'));await flush();
  assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads+1);assert.equal(document.querySelector('.view-recovery'),null);
  assert.equal(harness.viewport.selection.instance_id,'rail');assert.ok(document.querySelector('[aria-label="Show carriage"]'));assert.ok(harness.viewer.some(([name,direction])=>name==='view'&&direction==='front'));
  assert.equal(harness.calls.some(([name])=>['rebuild','repair_display','parameter','example'].includes(name)),false);
});

test('graphics restoration loads the newly focused document and exposes retry if renderer setup still fails',async()=>{
  const invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.motion={...other.motion,workspace_id:other.workspace_id};
  harness.invoke=async(command,args)=>{if(command==='new_scratchpad'||command==='project'&&args.workspace===other.workspace_id){harness.calls.push([command,args]);return structuredClone(other);}return invoke(command,args);};
  await act(async()=>harness.graphics('lost'));const loads=harness.viewer.filter(([name])=>name==='load').length;
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads);
  harness.load=async()=>{throw new Error('Restored renderer setup failed');};
  await act(async()=>harness.graphics('restored'));await flush();assert.match(document.querySelector('.view-recovery [role="alert"]').textContent,/Restored renderer setup failed/);assert.ok(button('Retry view'));
  harness.load=null;await click(button('Retry view'));assert.equal(harness.viewport.presented.workspace,other.workspace_id);assert.equal(document.querySelector('.view-recovery'),null);
});

test('missing display cache after graphics loss preserves logical intent until explicit restoration',async()=>{
  await click(button('rail'));await click(document.querySelector('[aria-label="Hide carriage"]'));
  await act(async()=>harness.graphics('lost'));const loads=harness.viewer.filter(([name])=>name==='load').length;
  harness.project.display_error={code:'DISPLAY_CACHE_MISSING',message:'Restore the display cache'};
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await act(async()=>harness.graphics('restored'));await flush();
  assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads,'An empty scene must not erase retained camera and selection intent');assert.ok(button('Restore display'));
  assert.equal(harness.viewport.presented,null);assert.equal(document.querySelector('.part-select[aria-pressed="true"]').textContent.trim(),'rail');assert.ok(document.querySelector('[aria-label="Show carriage"]'));
  await act(async()=>harness.menu.onmessage('front'));
  assert.equal(harness.calls.some(([name])=>name==='repair_display'||name==='rebuild'),false);
  delete harness.project.display_error;harness.project.display_generation='restored-display';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});await flush();
  assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads+1);assert.equal(harness.viewport.selection.instance_id,'rail');assert.ok(document.querySelector('[aria-label="Show carriage"]'));assert.equal(document.querySelector('.view-recovery'),null);assert.ok(harness.viewer.some(([name,direction])=>name==='view'&&direction==='front'));
});

async function restartWithGraphicsFailure(attempts=1){
  await act(async()=>root.unmount());harness.startupFailures=attempts;
  root=createRoot(document.getElementById('root'));await act(async()=>root.render(React.createElement(App)));await flush();
}

test('renderer startup failure keeps workspace commands available and explicit retry restores current preferences',async()=>{
  await restartWithGraphicsFailure(2);const attempts=harness.constructions,loads=harness.viewer.filter(([name])=>name==='load').length;
  assert.match(document.querySelector('.view-recovery [role="alert"]').textContent,/Synthetic WebGL initialization failure/);assert.ok(document.querySelector('[aria-label="Assistant"]'));
  await click(button('rail'));await click(document.querySelector('[aria-label="Hide carriage"]'));await act(async()=>{harness.menu.onmessage('select-edge');harness.menu.onmessage('advanced');harness.menu.onmessage('front');});
  await openMotion();await click(document.querySelector('[aria-label="Drag in canvas"]'));
  await act(async()=>harness.view.onmessage({id:'startup-dimensions',workspace_id:'workspace-1',revision:'r1',command:'dimensions',arguments:{visible:true,scope:'all'}}));
  assert.equal(harness.calls.findLast(([name])=>name==='complete_view')[1].reply.data.visible,true,'Display preferences can be queued before a renderer exists');
  await act(async()=>harness.view.onmessage({id:'startup-capture',workspace_id:'workspace-1',revision:'r1',command:'capture',arguments:{}}));
  assert.equal(harness.calls.findLast(([name])=>name==='complete_view')[1].reply.error.code,'VIEW_NOT_READY');assert.equal(harness.captureCalls||0,0);
  await act(async()=>{harness.menu.onmessage('save');harness.menu.onmessage('export-step');});await flush();
  assert.ok(harness.calls.some(([name])=>name==='save_project'));assert.ok(harness.calls.some(([name,args])=>name==='export'&&args.workspace==='workspace-1'&&args.revision==='r1'&&args.format==='STEP'));assert.equal(harness.constructions,attempts,'Document refresh must not loop graphics creation');
  await click(button('Retry view'));assert.equal(harness.constructions,attempts+1);assert.ok(button('Retry view'));assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads);
  const start=harness.viewer.length;await click(button('Retry view'));await flush();assert.equal(harness.constructions,attempts+2);assert.equal(document.querySelector('.view-recovery'),null);assert.equal(harness.viewport.selection.instance_id,'rail');
  const calls=harness.viewer.slice(start);assert.ok(calls.some(([name,visible,scope])=>name==='dimensions'&&visible&&scope==='all'));assert.ok(calls.some(([name,mode])=>name==='selectionMode'&&mode==='edge'));assert.ok(calls.some(([name,enabled])=>name==='advanced'&&enabled));assert.ok(calls.some(([name,enabled])=>name==='manipulation'&&enabled));assert.ok(calls.some(([name,direction])=>name==='view'&&direction==='front'));assert.ok(calls.some(([name,id,visible])=>name==='visible'&&id==='carriage'&&!visible));
  assert.equal(harness.calls.some(([name])=>['rebuild','repair_display','parameter','example'].includes(name)),false);
});

test('renderer startup retry loads the focused document and rejects stale callbacks from a disposed viewport',async()=>{
  const oldGraphics=harness.graphics;await restartWithGraphicsFailure();const invoke=harness.invoke.bind(harness),other=fixture();other.workspace_id='workspace-2';other.revision='r2';other.name='Second document';other.motion={...other.motion,workspace_id:other.workspace_id,revision:other.revision};
  harness.invoke=async(command,args)=>{if(command==='new_scratchpad'){harness.calls.push([command,args]);return structuredClone(other);}return invoke(command,args);};
  await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await act(async()=>harness.menu.onmessage('top'));const start=harness.viewer.length;
  await click(button('Retry view'));await flush();assert.equal(harness.viewport.presented.workspace,other.workspace_id);assert.equal(harness.viewport.presented.revision,other.revision);assert.ok(harness.viewer.slice(start).some(([name,direction])=>name==='view'&&direction==='top'));assert.equal(document.querySelector('.view-recovery'),null);
  await act(async()=>{oldGraphics('lost');oldGraphics('restored');});await flush();assert.equal(document.querySelector('.view-recovery'),null,'Disposed viewport events cannot overwrite a replacement');
});

test('cache cleanup explains display eviction and retains a matching presented scene on refresh',async()=>{
  const invoke=harness.invoke.bind(harness);harness.invoke=async(command,args)=>{if(command==='clean_cache'){harness.calls.push([command,args]);return {bytes_removed:1048576,already_running:false,retained_unmanaged:0,errors:[],evicted_display_revisions:2,display_budget_exceeded:true};}return invoke(command,args);};
  await act(async()=>harness.menu.onmessage('settings'));const storage=[...document.querySelectorAll('.settings-dialog details')].find(detail=>detail.querySelector('summary')?.textContent==='Storage');await act(async()=>{storage.open=true;});await click(button('Clean cache'));await click(document.querySelector('[aria-label="Close settings"]'));
  assert.match(document.querySelector('.notice').textContent,/Display data for 2 workspace/);assert.match(document.querySelector('.notice').textContent,/above the cache target/);
  const loads=harness.viewer.filter(([name])=>name==='load').length;harness.project.display_error={code:'DISPLAY_CACHE_MISSING',message:'Restore display data'};harness.project.display_generation='evicted';
  await act(async()=>harness.activity.onmessage({workspace_id:'workspace-1',kind:'refresh'}));await flush();assert.equal(harness.viewer.filter(([name])=>name==='load').length,loads);assert.ok(button('Restore display'));assert.equal(harness.calls.some(([name])=>name==='rebuild'||name==='repair_display'),false);
});


test('malformed conversation reply stays an actionable error instead of clearing the workspace',async()=>{
 const invoke=harness.invoke.bind(harness);
 const other={...fixture(),workspace_id:'workspace-2',name:'Second workspace'};
 harness.invoke=async(command,args)=>command==='agent_conversation'?{}:command==='new_scratchpad'?structuredClone(other):invoke(command,args);
 await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await flush();
 assert.ok(document.querySelector('.app'));
 assert.match(document.body.textContent,/conversation reply is invalid/);
});

test('malformed project reply preserves the current workspace and shows recovery guidance',async()=>{
 const invoke=harness.invoke.bind(harness);
 harness.invoke=async(command,args)=>command==='new_scratchpad'?{workspace_id:'bad',revision:'r2',model:{}}:invoke(command,args);
 await act(async()=>{harness.menu.onmessage('new');await new Promise(resolve=>setTimeout(resolve,10));});await flush();
 assert.ok(document.querySelector('.app'));
 assert.match(document.body.textContent,/workspace reply is invalid/);
 assert.match(document.querySelector('.document-name').textContent,/Rail assembly/);
});
test('viewport selection becomes lettered reference chips sent with the prompt',async()=>{
  harness.project.model.parts[0].source={file:'parts/rail.py',line:4};
  harness.project.model.parts[0].features={top:['face:2']};
  await act(async()=>root.unmount());
  root=createRoot(document.getElementById('root'));await act(async()=>root.render(React.createElement(App)));await flush();
  const chips=()=>[...document.querySelectorAll('.composer-references .reference-chip')].map(chip=>chip.textContent);
  const rail={revision:'r1',instance_id:'rail',entity_id:'face:2'},carriage={revision:'r1',instance_id:'carriage',entity_id:'part:0'};
  await act(async()=>{harness.select(rail);});
  assert.deepEqual(chips(),['railtop · parts/rail.py'],'A single selection is attached without a letter');
  await act(async()=>{harness.viewport.onSelect(carriage,'additive');});
  assert.deepEqual(chips(),['Arailtop · parts/rail.py','BcarriageWhole part']);
  assert.deepEqual(harness.viewport.selection,carriage,'The latest additive pick becomes the inspected selection');
  assert.ok(harness.calls.some(([name,args])=>name==='set_selection'&&args.entities.length===2),'External agents see both references');
  // Leaving one out keeps it selected but sends, and letters, only the rest.
  await click(document.querySelector('[aria-label="Leave rail top out of the message"]'));
  assert.deepEqual(chips(),['railtop · parts/rail.py↺','carriageWhole part']);
  await click(document.querySelector('[aria-label="Include rail top"]'));
  harness.viewport.capture=options=>{harness.viewportCapture=options;return {image:'iVBORw0KGgo='};};
  await submitPrompt('Make A flush with B');await flush();
  const start=harness.calls.find(([name])=>name==='agent_start')?.[1];
  assert.deepEqual(start.references,[rail,carriage]);
  assert.equal(start.image,'iVBORw0KGgo=');
  assert.equal(harness.viewportCapture.dimensions,false,'The reference screenshot stays uncluttered');
  // Sending clears the composer selection; rebuild it, then shift-click removes one pick.
  assert.equal(document.querySelector('.composer-references'),null);
  await act(async()=>{harness.select(rail);});
  await act(async()=>{harness.viewport.onSelect(carriage,'additive');});
  assert.deepEqual(chips(),['Arailtop · parts/rail.py','BcarriageWhole part']);
  await act(async()=>{harness.viewport.onSelect(carriage,'additive');});
  assert.deepEqual(chips(),['railtop · parts/rail.py']);
  await act(async()=>{harness.select(null);});
  assert.equal(document.querySelector('.composer-references'),null);
});

test('chat history searches date groups and supports keyboard selection and dismissal',async()=>{
  const today=new Date();today.setHours(12,0,0,0);
  const timestamp=days=>{const date=new Date(today);date.setDate(date.getDate()-days);return date.getTime();};
  const chats=[['current','Rail dimensions',0],['motor','Motor setup',1],['hinge','Hinge clearance',4],['old','First design',20]].map(([id,title,days])=>({id,title,created_at:timestamp(days),updated_at:timestamp(days),message_count:2}));
  let activeId='current';const original=harness.invoke.bind(harness),selections=[];
  const reply=()=>({active_chat_id:activeId,chats,messages:[{role:'assistant',text:`Conversation ${activeId}`}],turn:null});
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return reply();
    if(command==='agent_chat'){selections.push(args.chatId);activeId=args.chatId;return reply();}
    return original(command,args);
  };
  await act(async()=>harness.agent.onmessage({kind:'refresh'}));await flush();
  const trigger=()=>document.querySelector('[aria-label="Chat history"]');
  const popup=()=>document.querySelector('.chat-history-popover');
  await click(trigger());
  assert.equal(document.activeElement.getAttribute('aria-label'),'Search chats');
  assert.deepEqual([...popup().querySelectorAll('.chat-history-date')].map(node=>node.textContent),['Today','Yesterday','Previous 7 days','Older']);
  assert.equal(popup().querySelectorAll('[role="option"]').length,4);
  await setValue(popup().querySelector('input'),'not found');
  assert.match(popup().textContent,/No matching chats/);
  await setValue(popup().querySelector('input'),'motor');
  assert.equal(popup().querySelectorAll('[role="option"]').length,1);
  await act(async()=>popup().querySelector('input').dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'Enter',bubbles:true})));
  await flush();
  assert.deepEqual(selections,['motor']);assert.ok(!popup());
  assert.match(document.querySelector('.conversation').textContent,/Conversation motor/);
  await click(trigger());
  await act(async()=>document.activeElement.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true})));
  assert.equal(popup().querySelector('.highlighted').textContent,'Motor setup');
  await act(async()=>document.activeElement.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true})));
  assert.ok(!popup());assert.equal(document.activeElement,trigger());
  await click(trigger());
  await act(async()=>document.body.dispatchEvent(new dom.window.Event('pointerdown',{bubbles:true})));
  assert.ok(!popup());
});

test('assistant chat tabs can split beside each other like other panes',async()=>{
  const chats=[{id:'chat-1',title:'Rail dimensions',created_at:1,updated_at:2,message_count:2},{id:'chat-2',title:'Motor setup',created_at:3,updated_at:4,message_count:2}];
  const messages={'chat-1':[{role:'user',text:'Make the rail longer'},{role:'assistant',text:'Rail updated.'}],'chat-2':[{role:'user',text:'Set up the motor'},{role:'assistant',text:'Motor configured.'}]};
  chats.push(...Array.from({length:40},(_,index)=>({id:`history-${index}`,title:`History ${index}`,created_at:1,updated_at:1,message_count:100})));
  const reads=[];
  let activeId='chat-1';const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation'){
      if(args.chatId)reads.push(args.chatId);
      const id=args.chatId||activeId;
      return {active_chat_id:activeId,chats:structuredClone(chats),messages:structuredClone(messages[id]||[]),turn:null};
    }
    if(command==='agent_chat'){activeId=args.action==='select'?args.chatId:activeId;return {active_chat_id:activeId,chats:structuredClone(chats),messages:structuredClone(messages[activeId]||[]),turn:null};}
    return original(command,args);
  };
  await act(async()=>harness.agent.onmessage({kind:'refresh'}));await flush();
  assert.deepEqual(reads,[],'Hidden saved chats must not be fetched');
  assert.equal(document.querySelectorAll('.assistant-chat-pane').length,1,'Only a visible conversation is mounted');
  const dock=document.querySelector('#panel-assistant-dock .dock-area');
  assert.ok(dock);
  assert.equal(document.querySelectorAll('#panel-assistant-dock .dock-group').length,1);
  const tab=document.querySelector('#panel-assistant-dock .dock-tab [data-tab="chat-2"]')?.closest('.dock-tab');
  const body=document.querySelector('#panel-assistant-dock .dock-body');
  assert.ok(tab&&body);
  body.getBoundingClientRect=()=>({left:0,top:0,width:400,height:600,right:400,bottom:600,x:0,y:0,toJSON(){}});
  const transfer={types:['application/x-cadpad-tab'],getData:()=>JSON.stringify({tab:'chat-2',area:'chats'}),setData(){},effectAllowed:'move',dropEffect:'move'};
  await act(async()=>{
    const start=new dom.window.Event('dragstart',{bubbles:true,cancelable:true});
    Object.defineProperty(start,'dataTransfer',{value:transfer});
    tab.dispatchEvent(start);
    const over=new dom.window.Event('dragover',{bubbles:true,cancelable:true});
    Object.defineProperty(over,'dataTransfer',{value:transfer});
    Object.defineProperty(over,'clientX',{value:350});
    Object.defineProperty(over,'clientY',{value:300});
    body.dispatchEvent(over);
    const drop=new dom.window.Event('drop',{bubbles:true,cancelable:true});
    Object.defineProperty(drop,'dataTransfer',{value:transfer});
    Object.defineProperty(drop,'clientX',{value:350});
    Object.defineProperty(drop,'clientY',{value:300});
    body.dispatchEvent(drop);
    await new Promise(resolve=>setTimeout(resolve,20));
  });
  assert.ok(document.querySelector('#panel-assistant-dock .dock-split'),'Dropping a chat on a pane edge creates a split');
  assert.ok(document.querySelectorAll('#panel-assistant-dock .dock-group').length>=2);
  assert.match(document.querySelector('#panel-assistant-dock').textContent,/Rail updated/);
  assert.match(document.querySelector('#panel-assistant-dock').textContent,/Motor configured/);
  assert.deepEqual(reads,['chat-2'],'Splitting loads just the newly visible chat');
  const dropChat=async(body,x)=>{
    body.getBoundingClientRect=()=>({left:0,top:0,width:400,height:600,right:400,bottom:600});
    await act(async()=>{
      const event=new dom.window.Event('drop',{bubbles:true,cancelable:true});
      Object.defineProperties(event,{dataTransfer:{value:transfer},clientX:{value:x},clientY:{value:300}});
      body.dispatchEvent(event);
    });
    await flush();
  };
  for(let cycle=0;cycle<3;cycle++){
    const first=document.querySelector('#panel-assistant-dock .dock-body');
    await dropChat(first,200);
    await click(document.querySelector('#panel-assistant-dock [data-tab="chat-1"]'));
    assert.equal(document.querySelectorAll('.assistant-chat-pane').length,1,'Joining releases the hidden conversation DOM');
    assert.doesNotMatch(document.querySelector('#panel-assistant-dock').textContent,/Motor configured/);
    await dropChat(document.querySelector('#panel-assistant-dock .dock-body'),350);
    assert.equal(document.querySelectorAll('.assistant-chat-pane').length,2);
    assert.equal(reads.length,cycle+2,'Reopening a hidden pane reloads its evicted message bodies');
  }
});


test('dropping an image onto the assistant pane attaches it to the next prompt',async()=>{
  const originalCreate=globalThis.createImageBitmap;
  const originalGetContext=dom.window.HTMLCanvasElement.prototype.getContext;
  const originalToDataURL=dom.window.HTMLCanvasElement.prototype.toDataURL;
  globalThis.createImageBitmap=async()=>({width:8,height:8,close(){}});
  dom.window.HTMLCanvasElement.prototype.getContext=()=>({clearRect(){},drawImage(){}});
  dom.window.HTMLCanvasElement.prototype.toDataURL=()=>'data:image/png;base64,iVBORw0KGgo=';
  try{
    const pane=document.getElementById('panel-assistant-dock');
    assert.ok(pane);
    const file=new dom.window.File([Uint8Array.of(1,2,3)],'bracket.png',{type:'image/png'});
    const transfer={types:['Files'],files:[file],items:[{kind:'file',type:'image/png',getAsFile:()=>file}],dropEffect:'none'};
    const fire=(type)=>{const event=new dom.window.Event(type,{bubbles:true,cancelable:true});Object.defineProperty(event,'dataTransfer',{value:transfer});pane.dispatchEvent(event);};
    await act(async()=>{fire('dragenter');fire('dragover');await new Promise(resolve=>setTimeout(resolve,5));});
    assert.ok(pane.classList.contains('is-image-drop'));
    assert.match(pane.textContent,/Drop image to attach/);
    await act(async()=>{fire('drop');await new Promise(resolve=>setTimeout(resolve,30));});
    assert.ok(document.querySelector('.composer-image'));
    assert.match(document.querySelector('.composer-image').textContent,/bracket\.png/);
    await submitPrompt('Match this bracket silhouette');await flush();
    const start=harness.calls.findLast(([name])=>name==='agent_start')?.[1];
    assert.equal(start.image,'iVBORw0KGgo=');
    assert.deepEqual(start.references,[]);
    assert.equal(document.querySelector('.composer-image'),null,'Sending clears the attached image');
  }finally{
    if(originalCreate)globalThis.createImageBitmap=originalCreate;else delete globalThis.createImageBitmap;
    dom.window.HTMLCanvasElement.prototype.getContext=originalGetContext;
    dom.window.HTMLCanvasElement.prototype.toDataURL=originalToDataURL;
  }
});

test('outbound user messages stay visible while a streamed reply is in flight',async()=>{
  const invoke=harness.invoke.bind(harness);let turn=null,messages=[];
  const conversation=deferred();
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation'){harness.calls.push([command,args]);return conversation.promise;}
    if(command==='agent_start'){harness.calls.push([command,args]);turn={...agentTurn('stream-2','workspace-1'),steps:[],started_at:Date.now()};messages=[{role:'user',text:'First request'},{role:'assistant',text:'Done.'},{role:'user',text:args.prompt}];return turn;}
    return invoke(command,args);
  };
  await setValue(document.getElementById('model-prompt'),'');
  await act(async()=>{harness.agent.onmessage({kind:'refresh'});});
  // Seed the first exchange without racing the deferred conversation read.
  conversation.resolve({messages:[{role:'user',text:'First request'},{role:'assistant',text:'Done.'}],turn:null});
  await flush(20);
  const next=deferred();
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation'){harness.calls.push([command,args]);return next.promise;}
    if(command==='agent_start'){harness.calls.push([command,args]);turn={...agentTurn('stream-2','workspace-1'),steps:[],started_at:Date.now()};messages=[{role:'user',text:'First request'},{role:'assistant',text:'Done.'},{role:'user',text:args.prompt}];return turn;}
    return invoke(command,args);
  };
  await submitPrompt('Second request');
  assert.match(document.querySelector('.conversation').textContent,/Second request/,'The second prompt must appear before the conversation refresh settles');
  await act(async()=>{harness.agent.onmessage({kind:'turn_started',workspace_id:'workspace-1',turn_id:'stream-2',turn});});
  await act(async()=>{harness.agent.onmessage({kind:'text_delta',workspace_id:'workspace-1',turn_id:'stream-2',delta:'Working'});});
  await act(async()=>{next.resolve({messages,turn});await new Promise(resolve=>setTimeout(resolve,20));});
  await flush();
  assert.match(document.querySelector('.conversation').textContent,/Second request/);
  assert.equal([...document.querySelectorAll('.chat-message.user')].map(node=>node.textContent.replace(/^You/,'').trim()).at(-1),'Second request');
});

test('sending a message scrolls the conversation to the latest reply',async()=>{
  const invoke=harness.invoke.bind(harness);let turn=null,messages=[];
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return {messages,turn};
    if(command==='agent_start'){turn={...agentTurn('scroll-1','workspace-1'),steps:[],started_at:Date.now()};messages=[{role:'user',text:args.prompt}];return turn;}
    return invoke(command,args);
  };
  const conversation=document.querySelector('.conversation');
  Object.defineProperty(conversation,'scrollHeight',{configurable:true,get:()=>1200});
  Object.defineProperty(conversation,'clientHeight',{configurable:true,get:()=>400});
  conversation.scrollTop=0;
  await submitPrompt('Scroll me to the bottom');
  assert.equal(conversation.scrollTop,1200,'The conversation must jump to the bottom when a message is sent');
});

test('agent replies stream thinking, tool hints and markdown, then collapse into a finished reply',async()=>{
  const invoke=harness.invoke.bind(harness);let turn=null,messages=[];
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return {messages,turn};
    if(command==='agent_start'){harness.calls.push([command,args]);turn={...agentTurn('steps-1','workspace-1'),steps:[],started_at:Date.now()};messages=[{role:'user',text:args.prompt}];return turn;}
    return invoke(command,args);
  };
  const panel=()=>document.getElementById('panel-assistant');
  assert.match(panel().textContent,/Describe a change/,'The welcome shows for an empty conversation');
  await submitPrompt('Remove the floating cube');await flush();
  assert.doesNotMatch(panel().textContent,/Describe a change/,'The welcome hides once a conversation is under way');
  const send=async(kind,next,delta)=>{if(next)turn=next;await act(async()=>{harness.agent.onmessage({kind,workspace_id:'workspace-1',turn_id:'steps-1',turn:next||undefined,delta});await new Promise(resolve=>setTimeout(resolve,5));});};
  await send('step',{...turn,activity:'Thinking',steps:[{kind:'thinking',text:'Looking for',done:false}]});
  await send('thinking_delta',null,' the stop cube');
  assert.match(panel().querySelector('.step-thinking.is-active').textContent,/Thinking.*Looking for the stop cube/);
  await send('tool_started',{...turn,activity:'Reading the source',steps:[{kind:'thinking',text:'Looking for the stop cube',done:true},
    {kind:'text',text:"I'll open `model.py`."},{kind:'tool',id:'t1',name:'read_source',label:'Reading the source',detail:'Find the cube',state:'running'}]});
  await send('text_delta',null,'');
  const tool=panel().querySelector('.step-tool.is-running');
  assert.match(tool.textContent,/Reading the source….*Find the cube/);
  assert.equal(panel().querySelector('.markdown code').textContent,'model.py','Inline code renders as code, not backticks');
  assert.match(panel().querySelector('.agent-status').textContent,/Reading the source…/);
  // Finished: the reply text leads, and the work collapses into a summary.
  messages=[{role:'user',text:'Remove the floating cube'},{role:'assistant',text:"I'll open `model.py`.\n\nRemoved the **stop** cube.",
    steps:[{kind:'thinking',text:'Looking for the stop cube',done:true},{kind:'tool',name:'read_source',label:'Reading the source',state:'done'},{kind:'tool',name:'build',label:'Building geometry',state:'failed'}]}];
  await send('turn_finished',{...turn,state:'succeeded',activity:'Finished'});await flush();
  const reply=[...panel().querySelectorAll('.chat-message.assistant')].pop();
  assert.equal(reply.querySelector('details.step-summary summary').textContent,'2 steps · 1 needed another try');
  assert.equal(reply.querySelector('.markdown > p:last-child strong').textContent,'stop');
  assert.equal(panel().querySelector('.agent-status'),null,'No live status after the turn ends');
});

test('rebuild stays blocked while a build is queued and blocks overlapping model edits',async()=>{
  const invoke=harness.invoke.bind(harness);
  const rebuild=deferred();
  harness.invoke=async(command,args)=>{
    if(command==='rebuild'){
      harness.calls.push([command,args]);
      return rebuild.promise;
    }
    return invoke(command,args);
  };
  await act(async()=>harness.menu.onmessage('rebuild'));
  await flush();
  assert.equal(harness.calls.filter(([name])=>name==='rebuild').length,1);
  await act(async()=>harness.menu.onmessage('rebuild'));
  assert.equal(harness.calls.filter(([name])=>name==='rebuild').length,1,'Overlapping rebuild must not start a second build');
  await act(async()=>rebuild.resolve({id:'job-1',state:'queued',diagnostics:null,actor:'local-user'}));
  await flush();
  await act(async()=>harness.menu.onmessage('rebuild'));
  assert.equal(harness.calls.filter(([name])=>name==='rebuild').length,1,'Queued builds keep owning the workspace');
});

test('cold inspect recovery mismatch stays in the selection panel instead of Action needs attention',async()=>{
  const invoke=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='inspect'){
      harness.calls.push([command,args]);
      throw {code:'RECOVERY_GEOMETRY_CHANGED',message:'Rebuilding the committed inputs produced different geometry or publication metadata. The committed model is unchanged; create a new build revision to accept the new result',details:null};
    }
    return invoke(command,args);
  };
  await act(async()=>{harness.select({revision:'r1',instance_id:'rail',entity_id:'part:0'});});
  await flush(200);
  assert.equal(document.querySelector('.error-banner'),null,'Restored or automatic inspect must not raise the action banner');
  assert.match(document.getElementById('panel-model').textContent,/Exact inspection needs a new build revision/);
  assert.ok(button('Rebuild as new revision'));
});


test('precise joint positions clamp to travel limits and support unlimited joints',async()=>{
  await openMotion();
  const input=document.querySelector('[aria-label="slide position"]');
  await setValue(input,'250');
  await act(async()=>input.dispatchEvent(new dom.window.FocusEvent('focusout',{bubbles:true})));
  await flush();
  assert.equal(harness.calls.findLast(([name])=>name==='joint_coordinate')[1].request.coordinate,.15);
  harness.project.motion.joints[0].limits=null;
  harness.project.motion.pose_version=2;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(document.querySelector('[aria-label="slide"]'),null);
  assert.ok(document.querySelector('[aria-label="slide position"]'));
});

test('canvas manipulation follows live mode and replay instead of the tool toggle alone',async()=>{
  await openMotion();await click(document.querySelector('[aria-label="Drag in canvas"]'));
  assert.equal(harness.viewer.findLast(([name])=>name==='manipulation')[1],true);
  harness.project.motion={...harness.project.motion,pose_version:1,mode:'simulate',simulation:{mass_kg:{},current:{time_s:0}}};
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(harness.viewer.findLast(([name])=>name==='manipulation')[1],false);
  assert.match(document.querySelector('.motion-state').textContent,/Physics paused/);
  assert.ok(button('Stop and restore pose'));
  harness.project.motion={...harness.project.motion,pose_version:2,mode:'manipulate',playback:{name:'Saved movement',recording_id:'rec-1',time_s:0,duration_s:1,frame_index:0,frame_count:120}};
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  assert.equal(harness.viewer.findLast(([name])=>name==='manipulation')[1],false);
  assert.match(document.querySelector('.motion-state').textContent,/Replay/);
  assert.equal(document.querySelector('[aria-label="slide position"]').disabled,true);
});

test('physics without contact geometry explains the next step and prepares an assistant request',async()=>{
  harness.project.motion.collision=null;
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await openMotion();await chooseMotionTask('Physics');
  assert.ok(button('Set up with assistant'));
  await click(button('Set up with assistant'));
  assert.match(document.getElementById('model-prompt').value,/collision shapes/);
  assert.equal(harness.calls.some(([name])=>name==='agent_start'),false);
});

test('sidebar Assistant and Explorer toggles preserve the draft',async()=>{
  assert.ok(document.querySelector('.activity-rail #tab-assistant'));
  assert.ok(!document.querySelector('.app-header #tab-assistant'));
  assert.ok(document.querySelector('.activity-rail #tab-model'));
  assert.equal(document.querySelector('.viewport-tools #tab-model'),null);
  await setValue(document.getElementById('model-prompt'),'Keep this draft');
  await click(document.getElementById('tab-assistant'));
  assert.equal(document.getElementById('panel-assistant-dock').hidden,true);
  assert.equal(document.getElementById('tab-assistant').getAttribute('aria-expanded'),'false');
  await click(document.getElementById('tab-model'));
  assert.equal(document.getElementById('panel-model').hidden,true);
  await click(document.getElementById('tab-assistant'));
  assert.equal(document.getElementById('model-prompt').value,'Keep this draft');
  await click(button('Assistant'));
  assert.equal(document.activeElement.id,'model-prompt');
  await click(document.querySelector('[aria-label="Hide Assistant panel"]'));
  assert.equal(document.activeElement.id,'tab-assistant');
  await click(document.getElementById('tab-model'));
  assert.equal(document.getElementById('panel-model').hidden,false);
});

test('panel dividers resize in the expected direction, clamp, reset and retain sizes',async()=>{
  await openMotion();
  for(const [panel,axis,initial,next] of [['model','clientX',320,400],['assistant','clientX',320,260],['motion','clientX',320,400]]){
    if(panel==='model'&&document.getElementById('panel-model').hidden)await click(document.getElementById('tab-model'));
    if(panel==='motion')await openMotion();
    const handle=document.querySelector(`[aria-label="Resize ${panel} panel"]`);
    handle.setPointerCapture=()=>{};handle.hasPointerCapture=()=>false;
    handle.parentElement.getBoundingClientRect=()=>({width:initial,height:initial});
    handle.parentElement.parentElement.getBoundingClientRect=()=>({width:1200,height:900});
    const pointer=(type,position)=>{const event=new dom.window.MouseEvent(type,{bubbles:true,button:0,[axis]:position});Object.defineProperty(event,'pointerId',{value:1});handle.dispatchEvent(event);};
    await act(async()=>{pointer('pointerdown',100);pointer('pointermove',180);pointer('pointerup',180);});
    assert.equal(handle.getAttribute('aria-valuenow'),String(next));
    assert.equal(document.querySelector('.app').style.getPropertyValue(`--${panel+'-width'}`),`${next}px`);
  }
  assert.deepEqual(JSON.parse(localStorage.getItem('cadpad.panel-sizes.v2')),{model:400,assistant:260,motion:400});
  const modelHandle=document.querySelector('[aria-label="Resize model panel"]');
  await act(async()=>modelHandle.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'Home',bubbles:true})));
  assert.equal(modelHandle.getAttribute('aria-valuenow'),'260');
  await act(async()=>modelHandle.dispatchEvent(new dom.window.MouseEvent('dblclick',{bubbles:true})));
  assert.equal(modelHandle.getAttribute('aria-valuenow'),'320');
  await act(async()=>root.unmount());root=createRoot(document.getElementById('root'));
  await act(async()=>root.render(React.createElement(App)));await flush();
  assert.equal(document.querySelector('[aria-label="Resize assistant panel"]').getAttribute('aria-valuenow'),'260');
});

test('Assistant creates fresh chats and resumes old messages and per-chat drafts',async()=>{
  const chats=[{id:'chat-1',title:'Rail dimensions',created_at:1,updated_at:2,message_count:2},{id:'chat-2',title:'Motor setup',created_at:3,updated_at:4,message_count:2}];
  const messages={'chat-1':[{role:'user',text:'Make the rail longer'},{role:'assistant',text:'Rail updated.'}],'chat-2':[{role:'user',text:'Set up the motor'},{role:'assistant',text:'Motor configured.'}]};
  let activeId='chat-1';const original=harness.invoke.bind(harness);
  const reply=()=>({active_chat_id:activeId,chats:structuredClone(chats.filter(chat=>chat.message_count>0)),messages:structuredClone(messages[activeId]||[]),turn:null});
  const chatTab=title=>[...document.querySelectorAll('#panel-assistant-dock .dock-tab button[role=tab]')].find(b=>b.textContent.trim()===title);
  const newChat=()=>document.querySelector('#panel-assistant-dock [aria-label="New chat"]');
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return reply();
    if(command==='agent_chat'){
      harness.calls.push([command,args]);
      if(args.action==='new'){activeId='chat-3';messages[activeId]=[];}
      else activeId=args.chatId;
      return reply();
    }
    return original(command,args);
  };
  await act(async()=>harness.agent.onmessage({kind:'refresh'}));await flush();
  assert.match(document.querySelector('.conversation').textContent,/Rail updated/);
  assert.equal(document.querySelectorAll('#panel-assistant-dock .dock-tab').length,2);
  await setValue(document.getElementById('model-prompt'),'Unsaved rail follow-up');
  await click(newChat());
  assert.equal(document.querySelector('.conversation .chat-message'),null);
  assert.equal(document.getElementById('model-prompt').value,'');
  assert.ok(document.querySelector('.assistant-welcome'));
  assert.equal(document.querySelectorAll('#panel-assistant-dock .dock-tab').length,3,'The empty chat appears as its own tab');
  assert.equal(chatTab('New chat').getAttribute('aria-selected'),'true');
  await setValue(document.getElementById('model-prompt'),'Fresh chat draft');
  await click(chatTab('Rail dimensions'));
  assert.match(document.querySelector('.conversation').textContent,/Rail updated/);
  assert.equal(document.getElementById('model-prompt').value,'Unsaved rail follow-up');
  await click(chatTab('Motor setup'));
  assert.match(document.querySelector('.conversation').textContent,/Motor configured/);
  await setValue(document.getElementById('model-prompt'),'Continue with this motor');await click(document.querySelector('[aria-label="Send message"]'));await flush();
  assert.equal(harness.calls.findLast(([name])=>name==='agent_start')[1].chatId,'chat-2');
});

test('chat selection failures retain messages and draft, and running work prevents chat changes',async()=>{
  const original=harness.invoke.bind(harness);let running=null;
  const newChat=()=>document.querySelector('#panel-assistant-dock [aria-label="New chat"]');
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return {active_chat_id:'chat-1',chats:[{id:'chat-1',title:'Current chat',created_at:1,updated_at:1,message_count:1}],messages:[{role:'user',text:'Keep this conversation'}],turn:running};
    if(command==='agent_chat')throw new Error('Could not save chat');
    return original(command,args);
  };
  await act(async()=>harness.agent.onmessage({kind:'refresh'}));await flush();
  await setValue(document.getElementById('model-prompt'),'Keep this draft');await click(newChat());
  assert.match(document.querySelector('.conversation').textContent,/Keep this conversation/);assert.equal(document.getElementById('model-prompt').value,'Keep this draft');
  running={id:'busy-chat',chat_id:'chat-1',workspace_id:'workspace-1',state:'running',text:'',activity:'Working',artifacts:[]};
  await act(async()=>harness.agent.onmessage({kind:'turn_started',workspace_id:'workspace-1',turn_id:'busy-chat',turn:running}));
  assert.equal(newChat().disabled,true);
});

test('Motion labels movable and fixed parts and locates a named part in the canvas',async()=>{
  harness.project.motion.joints[0].child='carriage';
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  await openMotion();
  const list=document.querySelector('.motion-parts');
  assert.match(list.querySelector('.motion-other-parts').textContent,/railFixed/);
  assert.match(list.querySelector('.motion-control').textContent,/carriageSlide/);
  assert.equal(list.querySelectorAll('.motion-control').length,1);
  await click(document.querySelector('[aria-label="Locate carriage in canvas"]'));
  assert.ok(harness.viewer.some(([name,entity])=>name==='selectEntity'&&entity?.instance_id==='carriage'&&entity.entity_id==='part:0'));
  await click(document.querySelector('[aria-label="Drag in canvas"]'));
  assert.match(document.querySelector('.motion-parts').textContent,/Drag a teal part/);
});

test('removing the active chat clears history and Undo restores it without changing the model',async()=>{
  const summary={id:'chat-remove',title:'Rail conversation',created_at:1,updated_at:2,message_count:2};
  const history=[{role:'user',text:'Review the rail'},{role:'assistant',text:'Rail reviewed.'}];
  let activeId=summary.id,removed=false;
  const original=harness.invoke.bind(harness);
  const reply=()=>({active_chat_id:activeId,chats:removed?[]:[summary],messages:activeId===summary.id?history:[],turn:null});
  harness.invoke=async(command,args)=>{
    if(command==='agent_conversation')return reply();
    if(command==='agent_chat'){
      harness.calls.push([command,args]);
      if(args.action==='remove'){removed=true;activeId='empty-chat';}
      if(args.action==='restore')removed=false;
      if(args.action==='select')activeId=args.chatId;
      return reply();
    }
    return original(command,args);
  };
  await act(async()=>harness.agent.onmessage({kind:'refresh'}));await flush();
  await click(document.querySelector('[aria-label="Close Rail conversation tab"]'));
  assert.equal(document.querySelectorAll('#panel-assistant-dock .dock-tab button[role=tab]').length,1);
  assert.equal(document.querySelector('#panel-assistant-dock .dock-tab.active button[role=tab]').textContent.trim(),'New chat');
  assert.equal(document.querySelector('.conversation .chat-message'),null);
  assert.ok(document.querySelector('.assistant-welcome'));
  assert.ok(button('Undo'));
  await click(button('Undo'));
  assert.deepEqual([...document.querySelectorAll('#panel-assistant-dock .dock-tab button[role=tab]')].map(b=>b.textContent.trim()),['Rail conversation','New chat']);
  await click([...document.querySelectorAll('#panel-assistant-dock .dock-tab button[role=tab]')].find(b=>b.textContent.trim()==='Rail conversation'));
  assert.match(document.querySelector('.conversation').textContent,/Rail reviewed/);
  assert.equal(harness.calls.some(([name])=>['undo','rebuild','parameter'].includes(name)),false);
});

test('Move, Physics and Requirements open directly as exclusive sidebar panels',async()=>{
  const sidebar=document.getElementById('panel-motion');
  for(const [title,id] of Object.entries(motionIcon)){
    const icon=document.getElementById(id);
    assert.ok(icon.closest('.activity-rail'));
    await chooseMotionTask(title);
    assert.equal(sidebar.hidden,false);
    assert.equal(sidebar.getAttribute('aria-label'),title);
    assert.equal(sidebar.querySelector('.dock-heading>strong').textContent,title);
    assert.equal(document.getElementById('panel-model').hidden,true);
    assert.equal(document.querySelector('.assistant-panel').hidden,false);
    assert.equal(sidebar.closest('.canvas-stack'),null);
    assert.equal(sidebar.querySelector('.motion-task-picker'),null);
    assert.equal(document.querySelector('[aria-label="Resize motion panel"]').getAttribute('aria-orientation'),'vertical');
    await click(icon);assert.equal(sidebar.hidden,true);
  }
  await chooseMotionTask('Physics');
  await setValue(document.getElementById('physics-gravity'),'off','change');
  await chooseMotionTask('Requirements');await chooseMotionTask('Physics');
  assert.equal(document.getElementById('physics-gravity').value,'off');
  await click(document.getElementById('tab-model'));
  assert.equal(document.getElementById('panel-model').hidden,false);
  assert.equal(sidebar.hidden,true);
});

test('authoritative status recovers a missed assistant completion without overlapping polls',async()=>{
  const original=harness.invoke.bind(harness),status=deferred();let state=agentTurn('missed-completion','workspace-1'),reads=0;
  harness.invoke=async(command,args)=>{
    if(command==='agent_start'){harness.calls.push([command,args]);return state;}
    if(command==='agent_turn'){reads++;return status.promise;}
    if(command==='agent_conversation')return {messages:state.state==='succeeded'?[{role:'user',text:'Create the clamp'},{role:'assistant',text:'Created the clamp.'}]:[],turn:state};
    return original(command,args);
  };
  await submitPrompt('Create the clamp');
  assert.equal(document.querySelector('#panel-assistant-dock [aria-label="New chat"]').disabled,true);
  await flush(2100);assert.equal(reads,1);
  await flush(2100);assert.equal(reads,1,'A slow read cannot create a queue of status polls');
  state={...state,state:'succeeded',text:'Created the clamp.',activity:'Finished'};
  await act(async()=>status.resolve(state));await flush();
  assert.equal(document.querySelector('.chat-message.streaming'),null);
  assert.match(document.querySelector('.conversation').textContent,/Created the clamp/);
  assert.equal(document.querySelector('#panel-assistant-dock [aria-label="New chat"]').disabled,false);
  await setValue(document.getElementById('model-prompt'),'Refine the jaws');
  assert.equal(document.querySelector('[aria-label="Send message"]').disabled,false);
});

test('empty model offers one combined import action and project opening',async()=>{
  harness.project.model.parts=[];harness.project.parameters={};harness.project.motion.joints=[];
  await act(async()=>{harness.menu.onmessage('save');await new Promise(resolve=>setTimeout(resolve,10));});
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(['open_project','import_model'].includes(command)){harness.calls.push([command,args]);return {cancelled:true};}
    return original(command,args);
  };
  for(const [label,command] of [['Open project','open_project'],['Import model','import_model']]){
    await click(document.querySelector(`.empty [aria-label="${label}"]`));
    const call=harness.calls.findLast(([name])=>name===command);assert.ok(call);
    if(command!=='open_project')assert.deepEqual(call[1],{workspace:'workspace-1',revision:'r1'});
  }
  assert.equal(button('Describe a part'),undefined);
  assert.equal(document.querySelectorAll('.empty button').length,2);
});


test('sidebar defaults match and legacy default widths migrate without changing custom sizes',async()=>{
  for(const panel of ['model','assistant','motion'])assert.equal(document.querySelector(`[aria-label="Resize ${panel} panel"]`).getAttribute('aria-valuenow'),'320');
  await act(async()=>root.unmount());
  localStorage.removeItem('cadpad.panel-sizes.v2');
  localStorage.setItem('cadpad.panel-sizes',JSON.stringify({model:230,assistant:360,motion:420}));
  root=createRoot(document.getElementById('root'));await act(async()=>root.render(React.createElement(App)));await flush();
  assert.deepEqual(JSON.parse(localStorage.getItem('cadpad.panel-sizes.v2')),{model:320,assistant:320,motion:420});
});


test('coordinate test results show position error in display units against the permitted tolerance',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>command==='motion_tests'?{catalog:'coordinate-result',tests:[{id:'position',definition:{name:'Slide position',intent:'Reach 10 mm within 0.5 mm',actions:[],checks:[{type:'coordinate',joint_id:'slide',target:.01,unit:'m',tolerance:.0005}]},outcome:{passed:false,measurements:[{label:'slide: remaining tolerance',actual:-.0005,expected:0,unit:'m',passed:false}]},stale:false,frame_count:0,duration_s:1}],accuracy:'Approximate',snapshot:null}:original(command,args);
  await flush(2300);await openMotion();await chooseMotionTask('Requirements');
  assert.match(document.querySelector('.test-measurements').textContent,/Position errorMeasured 1 mm · maximum 0.5 mm/);
  assert.equal(button('Inspect replay'),undefined,'Unavailable replay does not create a disabled action');
});

test('entry chooser lists relative Python paths, filters and opens the selected file',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='open_project')return {choose_entry:{request_id:'folder-request',folder:'My design',entries:['main.py','parts/frame.py','parts/lever.py','helpers.py','models/rail.py','models/jaw.py','assembly.py']}};
    if(command==='open_project_entry'){harness.calls.push([command,args]);return {project:harness.project,job:{id:'opened',state:'succeeded'}};}
    return original(command,args);
  };
  await act(async()=>harness.menu.onmessage('open'));await flush();
  const dialog=document.querySelector('.project-entry-dialog');
  assert.equal(dialog.open,true);
  assert.match(dialog.textContent,/My design/);
  assert.equal(dialog.querySelector('input[value="main.py"]').checked,true);
  assert.equal(harness.calls.some(([name])=>name==='open_project_entry'),false);
  await setValue(dialog.querySelector('[aria-label="Find Python file"]'),'lever');
  assert.equal(dialog.querySelectorAll('input[type="radio"]').length,1);
  await click(dialog.querySelector('input[value="parts/lever.py"]'));
  await click(button('Open project'));
  assert.deepEqual(harness.calls.findLast(([name])=>name==='open_project_entry')[1],{requestId:'folder-request',entry:'parts/lever.py'});
  assert.equal(document.querySelector('.project-entry-dialog'),null);
});

test('cancelling entry selection preserves the current project and discards the pending folder',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='open_project')return {choose_entry:{request_id:'cancel-request',folder:'Other folder',entries:['one.py','two.py']}};
    if(command==='open_project_entry'){harness.calls.push([command,args]);return {cancelled:true};}
    return original(command,args);
  };
  await act(async()=>harness.menu.onmessage('open'));await flush();
  await click(button('Cancel'));
  assert.deepEqual(harness.calls.findLast(([name])=>name==='open_project_entry')[1],{requestId:'cancel-request',entry:null});
  assert.equal(document.querySelector('.project-entry-dialog'),null);
  assert.ok(document.body.textContent.includes('Rail assembly'));
});

test('configured projects open directly and entry failures stay visible in the chooser',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>command==='open_project'?{project:harness.project,job:{id:'opened',state:'succeeded'}}:original(command,args);
  await act(async()=>harness.menu.onmessage('open'));await flush();
  assert.equal(document.querySelector('.project-entry-dialog'),null);
  harness.invoke=async(command,args)=>{
    if(command==='open_project')return {choose_entry:{request_id:'retry-request',folder:'Design',entries:['main.py','other.py']}};
    if(command==='open_project_entry')throw {message:'The entry file changed. Open the folder again.'};
    return original(command,args);
  };
  await act(async()=>harness.menu.onmessage('open'));await flush();
  await click(button('Open project'));
  assert.equal(document.querySelector('.project-entry-dialog').open,true);
  assert.match(document.querySelector('.project-entry-dialog [role="alert"]').textContent,/entry file changed/);
});

async function mountWorkbench(){
  await act(async()=>root.unmount());
  root=createRoot(document.getElementById('root'));
  await act(async()=>{root.render(React.createElement(Workbench));});await flush(30);
}
const canvasTabs=()=>[...document.querySelectorAll('.dock-canvas [role="tab"]')];

test('dock tree moves and reorders tabs, collapses empty splits and bounds resizing',()=>{
  const a=docking.group(['model','move'],'model');
  let tree=docking.placeTab(a,'model',a.id,'bottom');
  assert.equal(tree.kind,'split');assert.equal(tree.axis,'vertical');
  assert.deepEqual(docking.groups(tree).map(g=>g.tabs),[['move'],['model']]);
  const target=docking.groups(tree)[0].id;
  tree=docking.placeTab(tree,'model',target,'center','move');
  assert.equal(tree.kind,'group');assert.deepEqual(tree.tabs,['model','move']);
  tree=docking.placeTab(tree,'tests',tree.id,'right');
  assert.equal(docking.resizeSplit(tree,tree.id,3).ratio,.85);
  assert.equal(docking.resizeSplit(tree,tree.id,-3).ratio,.15);
  const moved=docking.placeTab(tree,'move',docking.groups(tree)[1].id,'bottom');
  assert.equal(docking.groups(moved).flatMap(g=>g.tabs).filter(id=>id==='move').length,1);
  assert.equal(docking.validateDock({kind:'group',id:'g',tabs:['model','model','unknown'],active:'unknown'},new Set(['model'])).active,'model');
});

test('canvas tabs retain independent sessions and focus routes save to the selected model',async()=>{
  const original=harness.invoke.bind(harness),second=fixture();
  second.workspace_id='workspace-2';second.name='Clamp';second.revision='r2';second.motion={...second.motion,workspace_id:'workspace-2',revision:'r2'};
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad')return second;
    if(command==='project'&&args.workspace==='workspace-2')return structuredClone(second);
    return original(command,args);
  };
  await mountWorkbench();
  const first=harness.viewports.at(-1);
  await act(async()=>harness.menu.onmessage('new-model'));await flush(30);
  assert.deepEqual(canvasTabs().map(tab=>tab.textContent),['Rail assembly','Clamp']);
  assert.equal(document.querySelector('.document-caption')?.textContent,'Model');
  const created=harness.constructions;
  await click(canvasTabs()[0]);await flush();
  await act(async()=>harness.menu.onmessage('save'));await flush();
  assert.equal(harness.calls.findLast(([name])=>name==='save_project')[1].workspace,'workspace-1');
  assert.ok(harness.viewports.includes(first));
  assert.equal(harness.constructions,created,'switching tabs must retain the existing viewport');
  await click(canvasTabs()[1]);await flush();
  await act(async()=>harness.menu.onmessage('save'));await flush();
  assert.equal(harness.calls.findLast(([name])=>name==='save_project')[1].workspace,'workspace-2');
});

test('New Project replaces open model tabs while New Model adds another tab',async()=>{
  const original=harness.invoke.bind(harness);let opens=0;
  const second=fixture();second.workspace_id='workspace-2';second.name='Clamp';second.motion={...second.motion,workspace_id:'workspace-2'};
  const third=fixture();third.workspace_id='workspace-3';third.name='Bracket';third.motion={...third.motion,workspace_id:'workspace-3'};
  harness.invoke=async(command,args)=>{
    if(command==='new_scratchpad')return structuredClone(opens++?third:second);
    return original(command,args);
  };
  await mountWorkbench();
  await act(async()=>harness.menu.onmessage('new-model'));await flush(30);
  assert.deepEqual(canvasTabs().map(tab=>tab.textContent),['Rail assembly','Clamp']);
  await act(async()=>harness.menu.onmessage('new-project'));await flush(30);
  assert.deepEqual(canvasTabs().map(tab=>tab.textContent),['Bracket']);
  assert.match(document.querySelector('.workbench-header-slot')?.textContent||'',/Bracket/);
});

test('canvas right and down splits retain viewports, resize and close without deleting workspaces',async()=>{
  await mountWorkbench();
  const first=harness.viewports.at(-1);
  await act(async()=>harness.menu.onmessage('split-right'));await flush(20);
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,2);
  assert.ok(harness.viewports.includes(first));
  const separator=document.querySelector('[aria-label="Resize canvas split"]');
  await act(async()=>separator.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true})));
  assert.equal(document.querySelector('[aria-label="Resize canvas split"]').getAttribute('aria-valuenow'),'55');
  await click(document.querySelectorAll('.dock-canvas .dock-group')[1].querySelector('[role="tab"]'));await act(async()=>harness.menu.onmessage('split-down'));await flush(20);
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,3);
  await click(document.querySelectorAll('.dock-canvas .tab-close')[2]);await flush();
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,2);
  assert.equal(harness.calls.some(([name])=>/delete.*workspace|remove.*workspace/.test(name)),false);
});

test('sidebar sections collapse independently and move between sidebars without replacing content',async()=>{
  await mountWorkbench();
  assert.equal(document.querySelectorAll('.sidebar-sections[data-dock-area="left"] .sidebar-section').length,1,'activity rail keeps one exclusive left tool');
  assert.ok(!document.querySelector('.docked-sidebar > .dock-area > .dock-group > .dock-tabs [role="tablist"]'),'Unsplit sidebars use section headings; nested assistant tabs are allowed');
  const assistant=document.getElementById('model-prompt');
  await click(document.querySelector('[aria-label="Collapse Explorer section"]'));
  assert.equal(document.getElementById('sidebar-content-model').hidden,true);
  await click(document.getElementById('tab-motion'));await flush();
  assert.equal(document.querySelectorAll('.sidebar-sections[data-dock-area="left"] .sidebar-section').length,1);
  assert.ok(document.querySelector('.docked-sidebar.left [data-sidebar-section="move"]'));
  const target=document.querySelector('.sidebar-sections[data-dock-area="left"]');
  const event=new dom.window.Event('drop',{bubbles:true,cancelable:true});
  Object.defineProperty(event,'dataTransfer',{value:{types:['application/x-cadpad-tab'],getData:()=>JSON.stringify({tab:'assistant',area:'right'})}});
  await act(async()=>target.dispatchEvent(event));await flush();
  assert.ok(!document.querySelector('.docked-sidebar.right'),'Moving the last right tool closes the right sidebar');
  assert.ok(document.getElementById('model-prompt')===assistant,'Moving the assistant preserves its composer node');
  assert.ok(document.querySelector('.docked-sidebar.left [data-sidebar-section="assistant"]'));
});

test('split views keep separate selections and restore the saved tab and split layout',async()=>{
  await mountWorkbench();
  const first=harness.viewports.at(-1);
  await act(async()=>first.onSelect({revision:'r1',instance_id:'rail',entity_id:'part:0'}));await flush();
  await act(async()=>harness.menu.onmessage('split-right'));await flush(20);
  const second=harness.viewports.at(-1);
  assert.notEqual(first,second);
  await act(async()=>second.onSelect({revision:'r1',instance_id:'carriage',entity_id:'part:0'}));await flush();
  assert.match(document.querySelector('#panel-model .part-row.selected').textContent,/carriage/);
  await click(canvasTabs()[0]);await flush();
  assert.match(document.querySelector('#panel-model .part-row.selected').textContent,/rail/);
  await flush(230);
  const saved=JSON.parse(localStorage.getItem('cadpad.dock-layout.v2'));
  assert.equal(saved.canvas.kind,'split');
  assert.equal(saved.documents.length,2);
  await mountWorkbench();
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,2);
  assert.equal(canvasTabs().length,2);
});

test('closing the last canvas shows recent models and relaunch reopens the focused workspace',async()=>{
  const original=harness.invoke.bind(harness);let newCalls=0;
  harness.invoke=async(command,args)=>{if(command==='new_scratchpad'){newCalls++;return structuredClone(harness.project);}return original(command,args);};
  await mountWorkbench();
  await click(document.querySelector('.dock-canvas .tab-close'));await flush();
  assert.equal(canvasTabs().length,0);
  assert.match(document.querySelector('.dock-welcome')?.textContent||'',/Cadpad|Open folder|Recent/);
  assert.ok(document.querySelector('.dock-welcome-recent .dock-welcome-open'),'closed tabs should still offer recent workspaces');
  await flush(230);await mountWorkbench();
  assert.equal(canvasTabs().length,1,'relaunch should reopen the focused workspace instead of a blank splash');
  await act(async()=>harness.menu.onmessage('save'));await flush();
  assert.equal(newCalls,0);
  await click(document.querySelector('.dock-canvas .tab-close'));await flush();
  await click(button('New model'));await flush(30);
  assert.equal(newCalls,1);
  assert.equal(canvasTabs().length,1);
});

test('welcome recent list can remove one workspace or clear all',async()=>{
  const original=harness.invoke.bind(harness);
  const second={workspace_id:'workspace-2',name:'Clamp',running:0};
  harness.invoke=async(command,args)=>{
    if(command==='workspaces')return [{workspace_id:harness.project.workspace_id,name:harness.project.name,running:0},second];
    if(command==='forget_workspace'){harness.calls.push([command,args]);return;}
    if(command==='forget_workspaces'){harness.calls.push([command,args]);return 2;}
    return original(command,args);
  };
  await mountWorkbench();
  await click(document.querySelector('.dock-canvas .tab-close'));await flush();
  assert.equal(document.querySelectorAll('.dock-welcome-recent .dock-welcome-open').length,2);
  await click(document.querySelector('[aria-label="Remove Clamp from recent"]'));await flush();
  assert.equal(harness.calls.some(([name,args])=>name==='forget_workspace'&&args.workspace==='workspace-2'),true);
  assert.equal(document.querySelectorAll('.dock-welcome-recent .dock-welcome-open').length,1);
  await click(button('Clear all'));await flush();
  assert.equal(harness.calls.some(([name])=>name==='forget_workspaces'),true);
  assert.equal(document.querySelector('.dock-welcome-recent'),null);
});

test('tab keyboard navigation selects the next model without losing its assistant draft',async()=>{
  const original=harness.invoke.bind(harness),second=fixture();second.workspace_id='workspace-2';second.name='Clamp';second.motion={...second.motion,workspace_id:'workspace-2'};
  harness.invoke=async(command,args)=>command==='new_scratchpad'?second:original(command,args);
  await mountWorkbench();
  await setValue(document.getElementById('model-prompt'),'Draft for rail');
  await act(async()=>harness.menu.onmessage('new-model'));await flush(30);
  assert.equal(document.getElementById('model-prompt').value,'');
  await setValue(document.getElementById('model-prompt'),'Draft for clamp');
  await act(async()=>canvasTabs()[1].dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'ArrowLeft',bubbles:true})));await flush();
  assert.equal(canvasTabs()[0].getAttribute('aria-selected'),'true');
  assert.equal(document.getElementById('model-prompt').value,'Draft for rail');
});

test('clicking inside an inactive portaled canvas focuses its pane and viewport routing remains scoped',async()=>{
  const original=harness.invoke.bind(harness),second=fixture();second.workspace_id='workspace-2';second.name='Clamp';second.revision='r2';second.motion={...second.motion,workspace_id:'workspace-2',revision:'r2'};
  harness.invoke=async(command,args)=>command==='new_scratchpad'?second:original(command,args);
  await mountWorkbench();
  await act(async()=>harness.menu.onmessage('split-right'));await flush(20);
  await act(async()=>harness.menu.onmessage('new-model'));await flush(20);
  const panes=document.querySelectorAll('.dock-canvas .dock-group');
  await act(async()=>panes[0].querySelector('.workspace').dispatchEvent(new dom.window.Event('pointerdown',{bubbles:true})));await flush();
  assert.ok(panes[0].classList.contains('focused'));
  assert.equal(panes[0].querySelector('[role="tab"]').getAttribute('aria-selected'),'true');
  await act(async()=>harness.view.onmessage({id:'second-view',workspace_id:'workspace-2',revision:'r2',command:'dimensions',arguments:{visible:true,scope:'all'}}));await flush();
  const reply=harness.calls.findLast(([name,args])=>name==='complete_view'&&args.requestId==='second-view')[1].reply;
  assert.equal(reply.data.workspace_id,'workspace-2');
  assert.equal(reply.data.revision,'r2');
  assert.match(document.querySelector('.workbench-header-slot').textContent,/Rail assembly/);
});

test('pane headers omit add and split controls while native canvas splitting stays available',async()=>{
  await mountWorkbench();
  assert.equal(document.querySelector('.dock-open-menu'),null);
  assert.equal(document.querySelector('[aria-label^="Split canvas"]'),null);
  assert.equal(document.querySelector('[aria-label="Add sidebar panel"]'),null);
  await act(async()=>harness.menu.onmessage('split-right'));await flush();
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,2);
  assert.equal(document.querySelector('.docked-sidebar .dock-divider'),null);
});

test('moving a model tab collapses its old group and retains the actual canvas instance',async()=>{
  await mountWorkbench();
  const canvas=document.querySelector('.dock-canvas .workspace'),tab=canvasTabs()[0].getAttribute('data-tab');
  await act(async()=>harness.menu.onmessage('split-right'));await flush(20);
  const count=harness.constructions;
  const target=document.querySelectorAll('.dock-canvas .dock-body')[1];
  const event=new dom.window.Event('drop',{bubbles:true,cancelable:true});
  Object.defineProperty(event,'dataTransfer',{value:{types:['application/x-cadpad-tab'],getData:()=>JSON.stringify({tab,area:'canvas'})}});
  await act(async()=>target.dispatchEvent(event));await flush();
  assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,1);
  assert.equal(canvasTabs().length,2);
  assert.ok(document.querySelector('.dock-canvas .dock-body').contains(canvas));
  assert.equal(harness.constructions,count);
  await act(async()=>harness.menu.onmessage('close-tab'));await flush();
  assert.equal(canvasTabs().length,1);
});

test('entry setup creates an adapter for an export script without altering the selected source',async()=>{
  const original=harness.invoke.bind(harness);
  harness.invoke=async(command,args)=>{
    if(command==='open_project')return {choose_entry:{request_id:'adapter-request',folder:'Fender',entries:['front_fender.py']}};
    if(command==='project_entry_plan')return {direct:false,has_main:true,recommended:'build',source_hash:'a'.repeat(64),options:[{id:'build',label:'build()',available:true,source:'# Generated adapter\ndef build(context): pass\n'},{id:'needs_size',label:'needs_size(size)',available:false,reason:'Requires arguments: size',source:null}]};
    if(command==='open_project_entry'){harness.calls.push([command,args]);return {project:harness.project,job:{id:'adapter-build',state:'succeeded'}};}
    return original(command,args);
  };
  await act(async()=>harness.menu.onmessage('open'));await flush();
  assert.ok(button('Create entry point'));
  assert.equal(document.querySelector('[aria-label="Geometry builder"]').value,'build');
  assert.match(document.querySelector('.entry-adapter').textContent,/existing source files stay unchanged/);
  assert.ok(!document.querySelector('.entry-adapter pre'),'Generated Python is not displayed');
  assert.equal(harness.calls.some(([name])=>name==='open_project_entry'),false);
  await click(button('Create entry point'));
  assert.deepEqual(harness.calls.findLast(([name])=>name==='open_project_entry')[1],{requestId:'adapter-request',entry:'front_fender.py',adapter:'build'});
});

test('build failures are attached to the affected canvas with concise diagnostics and entry setup',async()=>{
  const original=harness.invoke.bind(harness);harness.project.external_tracking=true;
  harness.invoke=async(command,args)=>{
    if(command==='configure_project_entry'){harness.calls.push([command,args]);return {choose_entry:{request_id:'repair-entry',folder:'Fender',entries:['front_fender.py'],current_entry:'front_fender.py'}};}
    return original(command,args);
  };
  await mountWorkbench();
  const diagnostics={code:'BUILD_FAILED',message:"Operation not permitted: '/app/runs/source/out'",details:{traceback:'Traceback:\n  File "/app/runs/source/front_fender.py", line 343, in <module>\n    main()\n  File "/app/runs/source/front_fender.py", line 307, in main\n    OUT.mkdir()\nPermissionError: Operation not permitted',logs:''}};
  await act(async()=>harness.activity.onmessage({workspace_id:'workspace-1',kind:'job_failed',job:{id:'failed-export',state:'failed',actor:'local-user',diagnostics}}));await flush();
  const drawer=document.querySelector('.dock-canvas .workspace>.build-diagnostics');
  assert.ok(drawer);assert.equal(drawer.open,true);
  assert.match(drawer.querySelector('summary').textContent,/front_fender.py:307/);
  assert.match(drawer.querySelector('[role="alert"]').textContent,/tried to write files/);
  assert.equal(document.querySelector('.workspace-notices .build-diagnostics'),null);
  assert.equal(document.querySelector('.error-banner'),null);
  assert.equal(drawer.querySelector('.build-error-technical').open,false);
  await click(button('Set up entry point…'));await flush();
  assert.equal(harness.calls.findLast(([name])=>name==='configure_project_entry')[1].workspace,'workspace-1');
  assert.equal(document.querySelector('input[name="entry-point"]').value,'front_fender.py');
});

test('restoring a model with a failed latest build retains its model-scoped diagnostics',async()=>{
  const project=structuredClone(harness.project);
  project.activity=[{id:'previous-failure',state:'failed',diagnostics:{message:'Missing builder output'}}];
  harness.invoke=async(command,args)=>{
    if(command==='initialize'||command==='project')return project;
    if(command==='workspaces')return [{workspace_id:project.workspace_id,name:project.name,running:0}];
    if(command==='agent_conversation')return {messages:[],turn:null};
    return {id:'unused',state:'succeeded'};
  };
  await mountWorkbench();
  assert.match(document.querySelector('.build-diagnostics').textContent,/Missing builder output/);
});

async function dockPointer(target,type,x,y){
  await act(async()=>{const event=new dom.window.MouseEvent(type,{bubbles:true,cancelable:true,button:0,clientX:x,clientY:y});Object.defineProperty(event,'pointerId',{value:41});target.dispatchEvent(event);});
}
function dockBounds(element,left=100,top=100,width=600,height=400){element.getBoundingClientRect=()=>({left,top,width,height,right:left+width,bottom:top+height,x:left,y:top,toJSON(){}});}

test('pointer dragging a lone model tab onto an edge creates a split view with clear previews',async()=>{
  await mountWorkbench();
  const body=document.querySelector('.dock-canvas .dock-body'),old=document.elementFromPoint;dockBounds(body);
  document.elementFromPoint=()=>body;
  try{
    await dockPointer(canvasTabs()[0],'pointerdown',20,20);
    await dockPointer(window,'pointermove',110,300);
    assert.match(document.querySelector('.dock-drag-ghost').textContent,/Split left/);
    assert.ok(document.querySelector('.dock-canvas .dock-drop-preview.left'));
    assert.equal(document.querySelectorAll('.dock-drop-compass>span').length,5);
    await dockPointer(window,'pointerup',110,300);await flush(20);
    assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,2);
    assert.equal(canvasTabs().length,2);
    assert.equal(document.querySelector('.dock-drag-ghost'),null);
    assert.equal(document.body.classList.contains('dock-dragging'),false);
  }finally{document.elementFromPoint=old;}
});

test('dragging a compact sidebar heading moves its section and keeps its content',async()=>{
  await mountWorkbench();
  const original=document.getElementById('panel-model');
  const stack=document.querySelector('.sidebar-sections[data-dock-area="right"]');
  const section=document.querySelector('[data-sidebar-section="assistant"]');
  const target=section.querySelector('.sidebar-section-heading'),old=document.elementFromPoint;
  dockBounds(stack,900,100,300,400);dockBounds(section,900,100,300,400);dockBounds(target,900,100,300,40);
  document.elementFromPoint=()=>target;
  try{
    await dockPointer(document.querySelector('[aria-label="Collapse Explorer section"]'),'pointerdown',50,50);
    // Exact stack center — nearby compass bands would advertise a split.
    await dockPointer(window,'pointermove',1050,300);
    assert.match(document.querySelector('.dock-drag-ghost').textContent,/Move section/);
    await dockPointer(window,'pointerup',1050,300);await flush();
    assert.deepEqual([...document.querySelectorAll('.docked-sidebar.right [data-sidebar-section]')].map(section=>section.dataset.sidebarSection),['model','assistant']);
    assert.equal(document.getElementById('panel-model'),original);
    assert.equal(document.getElementById('sidebar-content-model').hidden,false);
  }finally{document.elementFromPoint=old;}
});

test('dragging a sidebar tool onto another pane edge splits that sidebar',async()=>{
  await mountWorkbench();
  const stack=document.querySelector('.sidebar-sections[data-dock-area="right"]');
  // Headings occupy the top strip; splits must still win on edge zones there.
  const target=document.querySelector('[data-sidebar-section="assistant"] .sidebar-section-heading');
  const old=document.elementFromPoint;dockBounds(stack,900,100,300,400);dockBounds(target,900,100,300,40);
  document.elementFromPoint=()=>target;
  try{
    await dockPointer(document.querySelector('[aria-label="Collapse Explorer section"]'),'pointerdown',50,50);
    await dockPointer(window,'pointermove',910,120);
    assert.match(document.querySelector('.dock-drag-ghost').textContent,/Split left/);
    assert.ok(document.querySelector('.sidebar-sections .dock-drop-preview.left'));
    await dockPointer(window,'pointerup',910,120);await flush();
    assert.equal(document.querySelectorAll('.dock-right > .dock-split > .dock-branch > .dock-group').length,2);
    assert.deepEqual([...document.querySelectorAll('.dock-right > .dock-split > .dock-branch > .dock-group > .dock-tabs [data-tab]')].map(tab=>tab.dataset.tab).sort(),['assistant','model']);
    assert.equal(document.querySelector('.docked-sidebar.left'),null);
    assert.ok(document.querySelector('.docked-sidebar.right .dock-divider'));
  }finally{document.elementFromPoint=old;}
});

test('dragging Move from the activity rail onto the Explorer pane creates a left split',async()=>{
  await mountWorkbench();
  const stack=document.querySelector('.sidebar-sections[data-dock-area="left"]');
  const heading=document.querySelector('[data-sidebar-section="model"] .sidebar-section-heading');
  assert.ok(stack);assert.ok(heading);
  const old=document.elementFromPoint;dockBounds(stack,80,80,280,420);dockBounds(heading,80,80,280,36);
  document.elementFromPoint=()=>heading;
  try{
    await dockPointer(document.getElementById('tab-motion'),'pointerdown',30,90);
    await dockPointer(window,'pointermove',330,200);
    assert.match(document.querySelector('.dock-drag-ghost')?.textContent||'',/Move/);
    assert.match(document.querySelector('.dock-drag-ghost')?.textContent||'',/Split right|Split below|Split left|Split above/);
    assert.ok(document.querySelector('.sidebar-sections .dock-drop-preview'));
    await dockPointer(window,'pointerup',330,200);await flush();
    assert.equal(document.querySelectorAll('.docked-sidebar.left .dock-group').length,2,'Move should split beside Model instead of replacing it');
    assert.deepEqual([...document.querySelectorAll('.docked-sidebar.left [data-tab]')].map(tab=>tab.dataset.tab).sort(),['model','move']);
    assert.ok(document.querySelector('.docked-sidebar.left .dock-divider'));
    assert.equal(document.getElementById('panel-model')?.closest('.docked-sidebar.left')!=null,true);
    assert.equal(document.getElementById('panel-move')?.closest('.docked-sidebar.left')!=null,true);
  }finally{document.elementFromPoint=old;}
});

test('stacked sidebar sections expose a vertical resize handle between expanded tools',async()=>{
  await mountWorkbench();
  const stack=document.querySelector('.sidebar-sections[data-dock-area="left"]');
  const model=document.querySelector('[data-sidebar-section="model"]');
  const old=document.elementFromPoint;dockBounds(stack,80,80,280,500);dockBounds(model,80,80,280,500);
  document.elementFromPoint=()=>model;
  try{
    // Stay outside edge bands and the docking-compass cross so this joins the accordion.
    await dockPointer(document.getElementById('tab-tests'),'pointerdown',30,160);
    await dockPointer(window,'pointermove',220,400);
    await dockPointer(window,'pointerup',220,400);await flush();
    const joined=document.querySelector('.sidebar-sections[data-dock-area="left"]');
    assert.ok(joined?.classList.contains('is-stacked'));
    assert.deepEqual([...joined.querySelectorAll('[data-sidebar-section]')].map(section=>section.dataset.sidebarSection),['model','test']);
    const resizer=joined.querySelector('[aria-label="Resize Explorer and Requirements"]');
    assert.ok(resizer);
    const modelSection=joined.querySelector('[data-sidebar-section="model"]');
    const testSection=joined.querySelector('[data-sidebar-section="test"]');
    dockBounds(modelSection,80,80,280,220);dockBounds(testSection,80,300,280,220);
    await dockPointer(resizer,'pointerdown',220,300);
    await dockPointer(window,'pointermove',220,360);await flush();
    const style=element=>Number(dom.window.getComputedStyle(element).getPropertyValue('--section-weight'));
    assert.ok(style(modelSection)>style(testSection));
    await dockPointer(window,'pointerup',220,360);await flush();
    await act(async()=>resizer.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'Enter',bubbles:true})));await flush();
    assert.equal(style(modelSection),1);
    assert.equal(style(testSection),1);
  }finally{document.elementFromPoint=old;}
});

test('Escape cancels a pointer drag without changing the split layout',async()=>{
  await mountWorkbench();
  const body=document.querySelector('.dock-canvas .dock-body'),old=document.elementFromPoint;dockBounds(body);document.elementFromPoint=()=>body;
  try{
    await dockPointer(canvasTabs()[0],'pointerdown',20,20);
    await dockPointer(window,'pointermove',690,300);
    assert.ok(document.querySelector('.dock-drop-preview.right'));
    await act(async()=>window.dispatchEvent(new dom.window.KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true})));
    await dockPointer(window,'pointerup',690,300);await flush();
    assert.equal(document.querySelectorAll('.dock-canvas .dock-group').length,1);
    assert.equal(canvasTabs().length,1);
    assert.equal(document.querySelector('.dock-drag-ghost'),null);
    assert.equal(document.body.classList.contains('dock-dragging'),false);
  }finally{document.elementFromPoint=old;}
});

test('pointer dragging a section can reopen a closed sidebar at the window edge',async()=>{
  await mountWorkbench();await click(document.getElementById('tab-assistant'));await flush();
  assert.ok(!document.querySelector('.docked-sidebar.right'),'Moving the last right tool closes the right sidebar');
  const body=document.querySelector('.dock-canvas .dock-body'),old=document.elementFromPoint;document.elementFromPoint=()=>body;
  try{
    await dockPointer(document.querySelector('[aria-label="Collapse Explorer section"]'),'pointerdown',60,20);
    await dockPointer(window,'pointermove',990,300);
    const target=document.querySelector('.dock-outer-target.right');assert.ok(target);dockBounds(target,970,0,30,600);
    await dockPointer(window,'pointermove',990,300);
    await dockPointer(window,'pointerup',990,300);await flush();
    assert.ok(document.querySelector('.docked-sidebar.right [data-sidebar-section="model"]'));
    assert.equal(document.querySelector('.docked-sidebar.left [data-sidebar-section="model"]'),null);
  }finally{document.elementFromPoint=old;}
});

test('whole groups preserve tab order and selection when moved to a new split',()=>{
  const a=docking.group(['a','b'],'b'),b=docking.group(['c'],'c');
  const tree={kind:'split',id:'root',axis:'horizontal',ratio:.4,first:a,second:b};
  const next=docking.placeTabs(tree,['a','b'],b.id,'bottom',undefined,'b');
  assert.deepEqual(docking.groups(next).map(g=>g.tabs),[['c'],['a','b']]);
  assert.equal(docking.groups(next)[1].active,'b');
  const rect={left:0,top:0,width:500,height:300};
  for(const [x,y,edge] of [[10,150,'left'],[490,150,'right'],[250,10,'top'],[250,290,'bottom'],[250,150,'center'],[280,150,'right'],[250,120,'top']])assert.equal(docking.dockEdgeAt(x,y,rect),edge);
});

test('pointer dragging cancels when the app loses focus and restores ordinary clicks',async()=>{
  await mountWorkbench();
  const body=document.querySelector('.dock-canvas .dock-body'),old=document.elementFromPoint;dockBounds(body);document.elementFromPoint=()=>body;
  try{
    await dockPointer(canvasTabs()[0],'pointerdown',20,20);
    await dockPointer(window,'pointermove',110,300);
    assert.ok(document.querySelector('.dock-drag-ghost'));
    await act(async()=>window.dispatchEvent(new dom.window.Event('blur')));
    await dockPointer(window,'pointerup',110,300);await flush();
    assert.equal(document.querySelector('.dock-drag-ghost'),null);
    assert.equal(document.body.classList.contains('dock-dragging'),false);
    assert.equal(canvasTabs().length,1);
    await click(document.querySelector('.dock-canvas .tab-close'));
    assert.equal(canvasTabs().length,0);
  }finally{document.elementFromPoint=old;}
});
