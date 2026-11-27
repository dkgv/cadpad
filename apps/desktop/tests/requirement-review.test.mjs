// CPU tests for the Requirements review UI (questions, review, results, findings); jsdom, mocked Tauri invoke, no browser.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import React,{act} from 'react';

const src=new URL('../src/',import.meta.url).pathname;
/** Bundle the review modules; `@tauri-apps/api/core` is replaced by a stub that calls `globalThis.__invoke`. */
async function load(){
 await mkdir(path.join(process.cwd(),'target'),{recursive:true});
 const directory=await mkdtemp(path.join(process.cwd(),'target/review-unit-'));
 const stub={name:'tauri-stub',setup(b){
  b.onResolve({filter:/^@tauri-apps\/api\/core$/},()=>({path:'tauri-stub',namespace:'stub'}));
  b.onLoad({filter:/.*/,namespace:'stub'},()=>({contents:'export const invoke=(...args)=>globalThis.__invoke(...args);',loader:'js'}));
 }};
 const viewer={name:'viewer-stub',setup(b){
  b.onLoad({filter:/requirements\.css$/},()=>({contents:'',loader:'js'}));
  b.onResolve({filter:/^@cadpad\/viewer$/},()=>({path:'viewer-stub',namespace:'viewer-stub'}));
  b.onLoad({filter:/.*/,namespace:'viewer-stub'},()=>({contents:`export class CadViewport {
   load(model, revision, artifact, workspace) { return globalThis.__replayViewer.load(model, revision, artifact, workspace); }
   applyRequirementMoves(moves) { globalThis.__replayViewer.apply(moves); }
   applyRequirementPoses(poses) { globalThis.__replayViewer.applyPoses(poses); }
   dispose() { globalThis.__replayViewer.dispose(); }
  }`,loader:'js'}));
 }};
 const result=await build({stdin:{contents:`export * from './requirement-review.ts';export * from './requirement-cards.tsx';export * from './requirement-catalog.ts';export * from './requirement-replay.tsx';`,resolveDir:src,loader:'ts'},bundle:true,format:'esm',platform:'node',write:false,external:['react','react-dom'],plugins:[stub,viewer]});
 const file=path.join(directory,'module.mjs');await writeFile(file,result.outputFiles[0].text);
 return {module:await import(pathToFileURL(file)),cleanup:()=>rm(directory,{recursive:true,force:true})};
}
function dom(){
 const window=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','Node','Event','MouseEvent','KeyboardEvent'])Object.defineProperty(globalThis,key,{value:window.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 return window;
}
const hashes={requirement:'r1',manifest:'m1',plan:'p1'};
const line=(over)=>({text:'door never hits frame',claim:'c1',span:null,inferred:false,because:null,...over});
const info=(over={})=>({id:'req-1',name:'Door opens',status:'draft',approval:'draft',text:'@[Door](cabinet.py#cabinet/door) opens to 110 degrees',draft_hash:'r1',draft_hashes:hashes,
 print:{title:'Door opens',sets:[],lines:[line({span:[0,10]})],numbers_chosen:null,details:['56 checks']},rendered:'Door opens\n  1. Door never hits frame\n',diagnostics:[],approved:null,last_result:null,rerun_required:false,...over});

test('replay loads the evaluated workspace and releases it when the preview closes',async()=>{
 const {module:m,cleanup}=await load();
 const window=dom();
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const trigger=document.createElement('button');
 document.body.append(trigger);
 trigger.focus();
 const calls=[],moves=[{op:'rotate',parts:['door'],axis:'z',angle_deg:20}];
 globalThis.__invoke=async(command,args)=>{
  calls.push([command,args]);
  if(command==='project')return {workspace_id:'preview',revision:'sample-r1',model:{parts:[]}};
  if(command==='artifact')return new ArrayBuffer(0);
 };
 globalThis.__replayViewer={
  load:async(model,revision,artifact,workspace)=>{calls.push(['load',{model,revision,workspace}]);await artifact('mesh');},
  apply:values=>calls.push(['moves',values]),
  dispose:()=>calls.push(['dispose']),
 };
 try{
  await act(async()=>root.render(React.createElement(m.RequirementReplayPanel,{
   replay:{requirement_id:'req-1',primitive_id:'c1',state_index:3,mode:'parameter',moves,view:{workspace:'preview',revision:'sample-r1',temporary:true}},
   onClose:()=>root.render(null),
  })));
  assert.equal(document.activeElement.getAttribute('aria-label'),'Close replay');
  assert.deepEqual(calls,[
   ['project',{workspace:'preview'}],
   ['load',{model:{parts:[]},revision:'sample-r1',workspace:'preview'}],
   ['artifact',{workspace:'preview',revision:'sample-r1',handle:'mesh'}],
   ['moves',moves],
  ]);
  await act(async()=>window.window.dispatchEvent(new window.window.KeyboardEvent('keydown',{key:'Escape'})));
  assert.equal(document.querySelector('[role="dialog"]'),null);
  assert.equal(document.activeElement,trigger);
  assert.deepEqual(calls.slice(-2),[['dispose'],['close_requirement_replay',{workspace:'preview'}]]);
 }finally{
  await act(async()=>root.unmount());
  await cleanup();
  window.window.close();
  delete globalThis.__replayViewer;
  delete globalThis.__invoke;
 }
});

test('simulation replay applies the selected sampled poses',async()=>{
 const {module:m,cleanup}=await load();
 const window=dom();
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const poses={shoe:{position:[1,2,3],quaternion:[0,0,0,1]}};
 const calls=[];
 globalThis.__invoke=async(command,args)=>{
  calls.push([command,args]);
  if(command==='project')return {workspace_id:'model',revision:'sample-r1',model:{parts:[]}};
  if(command==='artifact')return new ArrayBuffer(0);
 };
 globalThis.__replayViewer={
  load:async(model,revision,artifact,workspace)=>{calls.push(['load',{model,revision,workspace}]);await artifact('mesh');},
  apply:values=>calls.push(['moves',values]),
  applyPoses:values=>calls.push(['poses',values]),
  dispose:()=>calls.push(['dispose']),
 };
 try{
  await act(async()=>root.render(React.createElement(m.RequirementReplayPanel,{
   replay:{requirement_id:'req-1',primitive_id:'c1',state_index:3,mode:'simulation',moves:[],simulation_poses:poses,view:{workspace:'model',revision:'sample-r1',temporary:false}},
   onClose:()=>root.render(null),
  })));
  assert.deepEqual(calls.at(-1),['poses',poses]);
  assert.match(document.querySelector('.rq-replay header .muted').textContent,/Simulation sample/);
 }finally{
  await act(async()=>root.unmount());
  window.window.close();
  await cleanup();
 }
});

test('spans are Unicode scalar offsets: highlight with non-BMP characters',async()=>{
 const {module:m,cleanup}=await load();
 try{
  // 😀 and 𝒳 are one scalar each but two UTF-16 units.
  const text='😀 lid must close on 𝒳 box, and look flush';
  const scalars=Array.from(text);
  const at=(needle)=>{const index=scalars.join('').indexOf(needle);return Array.from(text.slice(0,index)).length;};
  const covered=[0,at(' close')];            // "😀 lid must"
  const inferred=[at('close'),at(' 𝒳')];     // "close on"
  const print={title:'t',sets:[],lines:[line({span:covered}),line({claim:'c2',span:inferred,inferred:true,because:'close on'})],numbers_chosen:null,details:[]};
  const segments=m.highlightSegments(text,print,[]);
  assert.equal(segments.map(s=>s.raw).join(''),text,'every character is kept exactly once');
  assert.deepEqual(segments.map(s=>[s.kind,s.raw]),[['covered','😀 lid must'],['plain',' '],['inferred','close on'],['uncovered',' 𝒳 box, and look flush']]);
  assert.equal(segments[0].claim,'c1');
  assert.equal(m.sliceScalars(text,0,3),'😀 l');
  assert.deepEqual(m.scalarOffsets('a😀b'),[0,1,3,4]);
  // Out-of-range and malformed spans never throw.
  assert.doesNotThrow(()=>m.highlightSegments('abc',{title:'t',sets:[],lines:[line({span:[2,99]}),line({span:[5,1]}),line({span:null})],details:[]},[]));
  assert.deepEqual(m.highlightSegments('',null,[]),[]);
  assert.deepEqual(m.highlightSegments('door opens',null,[]).map(s=>s.kind),['plain']);
  // A chip token is never split by a span that ends inside it.
  const chip='@[Door](cabinet.py#cabinet/door) opens';
  const split=m.highlightSegments(chip,{title:'t',sets:[],lines:[line({span:[0,8]})],details:[]},[]);
  assert.equal(split[0].raw,'@[Door](cabinet.py#cabinet/door)');assert.equal(split[0].kind,'covered');
  // Question quotes use the same offsets.
  const [q]=m.questionsOf({text,diagnostics:[{code:'E_VAGUE',severity:'error',at:'claims[0]',span:[at('close'),at(' on')],message:'What does close mean?',fix:{kind:'ask_user',options:['a','b']}}]});
  assert.equal(q.quote,'close');assert.deepEqual(q.options,['a','b']);
 }finally{await cleanup();}
});

test('results: null, malformed and wrapped last_result are all handled',async()=>{
 const {module:m,cleanup}=await load();
 try{
  for(const bad of [null,undefined,42,'x',[],{},{status:7},{body:{}}])assert.equal(m.readResult(bad),null);
  const body={status:'not_met',unsupported:1,claims:[{claim:'c1',rule:'r',status:'not_met'}],findings:[],primitives:[{id:'p1',claim:'c1',primitive:'overlap',status:'not_met',met:false,margin:{value:-1.4,unit:'mm'},worst:{state:55,state_label:'door_angle = 110 deg',pair:{a:['cabinet.py#cabinet/door'],b:['cabinet.py#cabinet/frame']},margin:{value:-1.4,unit:'mm'}},states:56,jobs:112,failing_states:[50,51,52,53,54,55],inconclusive:[],errors:[]}]};
  for(const raw of [body,{body,timings:{total_s:1.84,claims:{c1:1.8}}}]){
   const result=m.readResult(raw);
   assert.equal(result.status,'not_met');assert.equal(result.primitives[0].failing_states.length,6);
   assert.equal(m.headlinePrimitive(result).id,'p1');
  }
  assert.equal(m.readResult({body,timings:{total_s:1.84,claims:{}}}).timings.total_s,1.84);
  assert.equal(m.readResult({body,relink:{changes:[{message:'part renamed'}]}}).changes[0].message,'part renamed');
  assert.equal(m.formatQuantity({value:-1.4,unit:'mm'}),'-1.4 mm');assert.equal(m.formatQuantity({value:110,unit:'deg'}),'110°');assert.equal(m.formatQuantity({value:0.6,unit:'mm3'}),'0.6 mm³');assert.equal(m.formatQuantity(null),'');
  const strip=m.stateStrip({...body.primitives[0]});
  assert.equal(strip.length,56);assert.equal(strip.filter(c=>c.failing).length,6);
  assert.equal(m.stateStrip({states:1000,failing_states:[999]}).length,120);
  assert.equal(m.statusOf({status:null,approval:'approved'}),'pending');
 }finally{await cleanup();}
});

test('needs-review diff is plain language and finding selection tracks live ids',async()=>{
 const {module:m,cleanup}=await load();
 try{
  const approved={title:'t',sets:[],lines:[line({claim:'c1',text:'door never hits frame, checked every 2°'}),line({claim:'c2',text:'lid rests'})],numbers_chosen:'step 2°',details:[]};
  const draft={title:'t',sets:[],lines:[line({claim:'c1',text:'door never hits frame, checked every 1°'}),line({claim:'c3',text:'handle clears frame',inferred:true})],numbers_chosen:'step 1°',details:[]};
  const rows=m.printDiff(approved,draft);
  assert.deepEqual(rows.map(r=>r.sign),['~','+','-','~']);
  assert.match(rows[0].text,/every 2°.*every 1°/);assert.match(rows[1].text,/Inferred: handle clears frame/);assert.match(rows[2].text,/Removed: lid rests/);
  assert.deepEqual(m.printDiff(approved,approved),[]);assert.deepEqual(m.printDiff(null,draft),[]);
  const finding=(id,label)=>({id,claim:'c1',a:['m#b/clamp_2'],b:['m#b/rotor'],first_state:1,last_state:2,states:[{index:1,label:'angle = 126 deg'},{index:2,label:'angle = 132 deg'}],state_count:9,worst_value:{value:0.6,unit:'mm3'},label});
  let selected=m.toggleSelection([],'a');selected=m.toggleSelection(selected,'b');assert.deepEqual(selected,['a','b']);assert.deepEqual(m.toggleSelection(selected,'a'),['b']);
  assert.deepEqual(m.pruneSelection(['a','b','z'],[finding('a','new'),finding('b','resolved')]),['a'],'resolved and unknown ids drop out');
  assert.equal(m.findingStates(finding('a','new')),'angle = 126 deg to angle = 132 deg');
  const prompt=m.allowancePrompt({id:'req-1',name:'Clamps'},[finding('a','new'),finding('c','still_failing')]);
  assert.match(prompt,/finding a:/);assert.match(prompt,/finding c:/);assert.match(prompt,/clamp_2 and rotor/);assert.match(prompt,/Do not approve/);
  const grouped=m.groupByModel([{text:'@[A](a.py#a) next to @[B](b.py#b) and @[A](a.py#a)'},{text:'@[A](a.py#a) alone'},{text:'no chips'}]);
  assert.deepEqual([...grouped.keys()],[JSON.stringify([['a.py','a'],['b.py','b']]),JSON.stringify(['a.py','a']),'']);
  assert.deepEqual(m.modelsOfGroupKey(JSON.stringify([['a.py','a'],['b.py','b']])),[{entry:'a.py',builder:'a'},{entry:'b.py',builder:'b'}]);
  assert.deepEqual(m.modelsOfGroupKey(JSON.stringify(['a.py','a'])),[{entry:'a.py',builder:'a'}]);
  assert.deepEqual(m.modelsOfGroupKey(''),[]);
 }finally{await cleanup();}
});

test('review card: highlight, inferred marker, disabled Approve with reason, approve with reviewed hashes',async()=>{
 const {module:m,cleanup}=await load();
 const window=dom();const calls=[];
 globalThis.__invoke=async(command,args)=>{
  calls.push([command,args]);
  if(command==='requirements')return {catalog:'cat-1',requirements:[current]};
  if(command==='approve_requirement'){current={...current,approval:'approved',status:null};return {catalog:'cat-2',requirements:[current]};}
  throw new Error('unexpected '+command);
 };
 const text='@[Door](cabinet.py#cabinet/door) opens to 110 degrees 😀 and looks nice';
 const scalarSpan=(a,b)=>[Array.from(text.slice(0,text.indexOf(a))).length,Array.from(text.slice(0,text.indexOf(b)+b.length)).length];
 let current=info({text,print:{title:'Door opens',sets:[],lines:[line({span:scalarSpan('@[Door]','degrees')}),line({claim:'c2',text:'door rests on frame',inferred:true,because:'opens',span:null})],numbers_chosen:'door_angle step 2°',details:['56 checks · exact']}});
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const asks=[];
 const Harness=()=>{
  const catalog=m.useRequirementCatalog('proj');
  const actions={busy:catalog.busy,agentBusy:false,run:catalog.run,approve:i=>void catalog.approve(i),remove:()=>{},start:()=>{},cancel:()=>{},ask:(p)=>{asks.push(p);},editWording:()=>{},replay:async()=>{}};
  return React.createElement('div',null,catalog.requirements.map(item=>React.createElement(m.RequirementCard,{key:item.id,info:item,actions})));
 };
 const error=console.error;console.error=()=>{};
 try{
  await act(async()=>root.render(React.createElement(Harness)));
  await act(async()=>{await new Promise(r=>setTimeout(r,20));});
  const root_=document.getElementById('root');
  // Covered / inferred (marked inline) / not checked.
  assert.ok(root_.querySelector('.rq-prose .cov:not(.inf)'));
  assert.equal(root_.querySelector('.rq-prose .unc').textContent.trim(),'😀 and looks nice');
  assert.match(root_.querySelector('.rq-callout.warn').textContent,/won’t be checked/);
  assert.ok(root_.querySelector('.rq-print .rq-src.inf'),'inferred rule is marked inline');
  assert.match(root_.textContent,/Numbers I chose:\s*door_angle step 2°/);
  assert.match(root_.textContent,/Approving also confirms the inferred rule/);
  assert.equal(root_.querySelectorAll('.rq-prose .req-chip').length,1);
  const approve=[...root_.querySelectorAll('button')].find(b=>b.textContent==='Approve');
  assert.equal(approve.disabled,false);
  // Disabled, with the reason shown, while an error exists or hashes are missing.
  current={...current,diagnostics:[{code:'E_BIND_PART_MISSING',severity:'error',at:'claims[0]',message:'Part frame not found',fix:null}]};
  await act(async()=>{await new Promise(r=>setTimeout(r,2100));});
  const blocked=[...root_.querySelectorAll('button')].find(b=>b.textContent==='Approve');
  assert.equal(blocked.disabled,true);assert.match(root_.querySelector('#block-req-1').textContent,/Part frame not found/);
  assert.equal(blocked.getAttribute('aria-describedby'),'block-req-1');
  assert.equal(m.approveBlock(info({draft_hashes:null})).startsWith('The reviewed version is not available'),true);
  // Approve sends exactly the reviewed hashes and the catalog fingerprint.
  current={...current,diagnostics:[]};
  await act(async()=>{await new Promise(r=>setTimeout(r,2100));});
  await act(async()=>{[...root_.querySelectorAll('button')].find(b=>b.textContent==='Approve').click();});
  await act(async()=>{await new Promise(r=>setTimeout(r,20));});
  const approval=calls.find(([command])=>command==='approve_requirement');
  assert.deepEqual(approval[1],{request:{workspace_id:'proj',expected_catalog:'cat-1',id:'req-1',hashes}});
  // Approved with no result yet: null last_result renders without throwing.
  assert.ok(root_.querySelector('.rq-card'));assert.doesNotMatch(root_.textContent,/Approve$/);
  assert.match(root_.textContent,/No result yet|Not checked yet/);
  assert.ok([...root_.querySelectorAll('button')].some(b=>b.textContent==='Check now'));
  await act(async()=>root.unmount());
 }finally{try{await act(async()=>root.unmount());}catch{}console.error=error;window.window.close();await cleanup();}
});

test('prose advisory starts only when the visible review intersects, deduplicates, and never blocks approval',async()=>{
 const {module:m,cleanup}=await load();
 const window=dom(),calls=[];
 let resolveAdvisory;
 globalThis.__invoke=async(command,args)=>{
  calls.push([command,args]);
  if(command==='requirements')return {catalog:'cat-1',requirements:[current],run:null};
  if(command==='cross_check_requirement')return new Promise(resolve=>{resolveAdvisory=resolve;});
  throw new Error('unexpected '+command);
 };
 let current=info();
 const observations=[];
 globalThis.IntersectionObserver=class{
  constructor(callback){this.callback=callback;observations.push(this);}
  observe(element){this.element=element;}
  disconnect(){}
  intersect(ratio=1){this.callback([{isIntersecting:ratio>0,intersectionRatio:ratio,target:this.element}]);}
 };
 const oldRect=window.window.HTMLElement.prototype.getBoundingClientRect;
 window.window.HTMLElement.prototype.getBoundingClientRect=function(){return this.classList?.contains('rq-advisory')?{width:300,height:1,top:0,left:0,right:300,bottom:1,x:0,y:0,toJSON(){}}:oldRect.call(this);};
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const Harness=()=>{
  const catalog=m.useRequirementCatalog('proj');
  const actions={busy:catalog.busy,agentBusy:false,run:catalog.run,approve:()=>{},remove:()=>{},start:()=>{},cancel:()=>{},crossCheck:i=>catalog.crossCheck(i,'proj'),ask:()=>{},editWording:()=>{},replay:async()=>{}};
  return React.createElement(m.RequirementCard,{info:current,actions});
 };
 const render=async()=>act(async()=>root.render(React.createElement(Harness)));
 try{
  await render();
  await act(async()=>{await new Promise(r=>setTimeout(r,20));});
  assert.equal(calls.filter(([command])=>command==='cross_check_requirement').length,0,'mounting alone does not trigger the pass');
  assert.equal(observations.length,1);
  await act(async()=>{observations[0].intersect(0);await Promise.resolve();});
  assert.equal(calls.filter(([command])=>command==='cross_check_requirement').length,0,'zero-area intersections are ignored');
  await act(async()=>{observations[0].intersect(1);observations[0].intersect(1);await Promise.resolve();});
  assert.equal(calls.filter(([command])=>command==='cross_check_requirement').length,1,'repeated observer events share one request');
  assert.deepEqual(calls.find(([command])=>command==='cross_check_requirement')[1],{workspace:'proj',id:'req-1',draftHash:'r1',draftHashes:hashes});
  const approve=()=>[...document.querySelectorAll('button')].find(button=>button.textContent==='Approve');
  assert.equal(approve().disabled,false,'approval remains enabled while the advisory runs');
  await act(async()=>resolveAdvisory({available:true,draft_hash:'r1',draft_hashes:{plan:'p1',manifest:'m1',requirement:'r1'},warnings:['Print omits the requested opening direction.']}));
  assert.match(document.querySelector('.rq-advisory .rq-callout.warn').textContent,/opening direction/);

  current={...current,draft_hash:'r2',draft_hashes:{requirement:'r2',manifest:'m2',plan:'p2'},rendered:'Door opens without hitting frame\n  Updated print\n'};
  await render();
  assert.doesNotMatch(document.querySelector('.rq-card').textContent,/opening direction/,'the previous draft result is not shown for the new identity');
  assert.equal(approve().disabled,false);
  assert.equal(observations.length,2);
  await act(async()=>{observations[1].intersect(1);await Promise.resolve();});
  assert.equal(calls.filter(([command])=>command==='cross_check_requirement').length,2);
  assert.deepEqual(calls.filter(([command])=>command==='cross_check_requirement')[1][1],{workspace:'proj',id:'req-1',draftHash:'r2',draftHashes:{requirement:'r2',manifest:'m2',plan:'p2'}});
 }finally{
  await act(async()=>root.unmount());await cleanup();
  window.window.HTMLElement.prototype.getBoundingClientRect=oldRect;
  delete globalThis.IntersectionObserver;delete globalThis.__invoke;window.window.close();
 }
});

test('cards: needs-review diff + Re-approve, results, inconclusive, findings accept as allowance, questions',async()=>{
 const {module:m,cleanup}=await load();
 const window=dom();
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const asks=[],approved=[],started=[],replayed=[];
 const actions={busy:null,agentBusy:false,run:null,approve:i=>approved.push(i.id),remove:()=>{},start:i=>started.push(i.id),cancel:()=>{},ask:async p=>{asks.push(p);},editWording:()=>{},show:async()=>{},replay:async(...args)=>{replayed.push(args);}};
 const render=async(node)=>act(async()=>root.render(node));
 const host=()=>document.getElementById('root');
 const button=(label)=>[...host().querySelectorAll('button')].find(b=>b.textContent.startsWith(label));
 const error=console.error;console.error=()=>{};
 try{
  const old={title:'t',sets:[],lines:[line({claim:'c1',text:'door never hits frame, checked every 2°'})],numbers_chosen:null,details:[]};
  const edited=info({status:'needs_review',approval:'edited_since_approval',approved:{hashes,rendered:'Door opens\n',print:old},print:{...old,lines:[line({claim:'c1',text:'door never hits frame, checked every 1°'})]},last_result:{status:'met',body:undefined,primitives:[]}});
  await render(React.createElement(m.RequirementCard,{info:edited,actions}));
  assert.ok(host().querySelector('table.rq-diff'));assert.match(host().querySelector('table.rq-diff').textContent,/every 2°.*every 1°/);
  await act(async()=>button('Re-approve').click());assert.deepEqual(approved,['req-1']);

  // Relink-driven needs_review (approved, nothing edited): reasons + changes in plain words, nothing re-ran.
  const relinked=info({status:'needs_review',approval:'approved',approved:{hashes,rendered:'x',print:old},last_result:{status:'met',changes:[{kind:'set_membership',effect:'needs_review',at:'s',message:'New part tower_arm_6 now matches tower_arm_*'}],primitives:[{id:'p',claim:'c1',primitive:'x',status:'met',margin:{value:1.2,unit:'mm'},states:3,failing_states:[],inconclusive:[],errors:[]}]}});
  await render(React.createElement(m.RequirementCard,{info:relinked,actions}));
  assert.match(host().textContent,/New part tower_arm_6/);assert.match(host().textContent,/did not re-check/);assert.match(host().textContent,/Last result \(approved version\): Met, margin 1\.2 mm/);

  // Met, not met, inconclusive, pending (previous result stays visible).
  const result=(status,extra={})=>({status,unsupported:0,claims:[],findings:[],primitives:[{id:'p',claim:'c1',primitive:'overlap',status,margin:{value:status==='not_met'?-1.4:0.8,unit:'mm'},worst:{state:2,state_label:'door_angle = 88 deg',pair:{a:['a.py#a/handle'],b:['a.py#a/frame']},margin:{value:status==='not_met'?-1.4:0.8,unit:'mm'}},states:10,jobs:20,failing_states:status==='not_met'?[8,9]:[],inconclusive:[],errors:[]}],...extra});
  const done=(status,extra,over={})=>info({status,approval:'approved',approved:{hashes,rendered:'x',print:old},last_result:{...result(status,extra),timings:{total_s:1.8,claims:{c1:1.8}}},...over});
  await render(React.createElement(m.RequirementCard,{info:done('met'),actions}));
  assert.match(host().querySelector('.rq-meter').textContent,/0\.8 mm\s*spare at the tightest point/);assert.match(host().textContent,/Tightest at door_angle = 88 deg/);
  assert.match(host().querySelector('details.rq-fine').textContent,/1\.8 s/);
  await render(React.createElement(m.RequirementCard,{info:done('not_met'),actions}));
  assert.match(host().textContent,/Fails at 2 of 10 positions, missing by 1\.4 mm/);assert.equal(host().querySelectorAll('.rq-strip i.f').length,2);assert.ok(button('Show in viewer'));
  await render(React.createElement(m.RequirementCard,{info:done('inconclusive',{primitives:[{...result('inconclusive').primitives[0],inconclusive:[{state:3,state_label:'angle = 9 deg',pair:{a:['a.py#a/lid'],b:['a.py#a/box']},reason:'time limit'}]}]}),actions}));
  assert.match(host().textContent,/could not decide/);assert.match(host().textContent,/nothing is claimed/);assert.match(host().textContent,/lid and box at angle = 9 deg/);
  await render(React.createElement(m.RequirementCard,{info:done('met',{},{status:'pending'}),actions:{...actions,run:{id:'run-1',state:'queued'}}}));
  assert.match(host().querySelector('.rq-run').textContent,/Queued/);assert.match(host().textContent,/Previous result/);assert.ok(host().querySelector('.rq-previous'));assert.equal(button('Check now'),undefined);
  assert.ok(host().querySelector('[role=status] .spin'));
  const overBudget=done('met',{reasons:['This requirement is over the automatic run budget; check it on demand.']},{status:'pending'});
  await render(React.createElement(m.RequirementCard,{info:overBudget,actions:{...actions,run:null}}));
  assert.equal(host().querySelector('.rq-run'),null);assert.equal(host().querySelector('[role=status] .spin'),null);
  assert.match(host().textContent,/over the automatic run budget/);assert.equal(button('Check now').disabled,false);

  // Findings: select two, accept as allowances in one batch request.
  const finding=(id,label,b)=>({id,claim:'c1',a:['m#b/clamp'],b:[`m#b/${b}`],first_state:0,last_state:0,states:[{index:0,label:'angle = 48 deg'}],state_count:5,worst_value:{value:2.1,unit:'mm3'},label});
  const clashes=done('not_met',{findings:[finding('f1','new','rotor'),finding('f2','still_failing','comb'),finding('f3','resolved','base')]});
  await render(React.createElement(m.RequirementCard,{info:clashes,actions}));
  const showFinding=host().querySelector('.rq-finding button.rq-mini');assert.ok(showFinding);
  await act(async()=>{showFinding.click();await new Promise(resolve=>setTimeout(resolve,0));});
  assert.equal(replayed[0][1],'c1');assert.equal(replayed[0][2].state,0);assert.equal(replayed[0][2].state_label,'angle = 48 deg');
  const boxes=[...host().querySelectorAll('.rq-finding input[type=checkbox]')];
  assert.equal(boxes.length,2,'resolved findings cannot be selected');
  assert.equal(button('Accept'),undefined);
  await act(async()=>boxes[0].click());await act(async()=>boxes[1].click());
  assert.equal(button('Accept 2 as allowances').disabled,false);
  await act(async()=>button('Accept 2 as allowances').click());
  assert.equal(asks.length,1);assert.match(asks[0],/finding f1/);assert.match(asks[0],/finding f2/);assert.doesNotMatch(asks[0],/finding f3/);
  assert.match(host().querySelector('.rq-callout.blue').textContent,/one draft/);
  assert.match(host().textContent,/resolved/);

  // Questions: one at a time, suggested first, answers go to the assistant in one message.
  const text='ensure @[Turntable](t.py#t) can rotate freely 😀 without clashing';
  const start=Array.from(text.slice(0,text.indexOf('freely'))).length;
  const diagnostics=[{code:'E_VAGUE',severity:'error',at:'claims[0]',span:[start,start+6],message:'freely is vague',fix:{kind:'ask_user',question:'What does "freely" mean?',options:['It never touches the base','It turns without a motor']}},
   {code:'E_VAGUE',severity:'error',at:'claims[1]',span:[0,6],message:'ensure is vague',fix:{kind:'ask_user',question:'Second question?',options:['Yes','No']}}];
  const drafted=info({text,diagnostics,print:null,rendered:null});
  const qs=m.questionsOf(drafted);assert.equal(qs.length,2);assert.equal(qs[0].quote,'freely');
  await render(React.createElement(m.QuestionFlow,{info:drafted,questions:qs,agentBusy:false,onSend:async p=>{asks.push(p);}}));
  assert.match(host().textContent,/1 of 2/);assert.equal(host().querySelector('mark').textContent,'freely');
  const radios=[...host().querySelectorAll('input[type=radio]')];
  assert.equal(radios[0].checked,true,'the suggestion is pre-selected');assert.match(host().querySelector('.rq-suggested').textContent,/Suggested/);
  assert.equal(radios.at(-1).value,'__other__','includes an Other option');
  assert.ok(button('Use suggested for the rest'));
  await act(async()=>button('Next').click());
  assert.match(host().textContent,/2 of 2/);assert.equal(asks.length,1,'nothing is sent until the last answer');
  await act(async()=>[...host().querySelectorAll('input[type=radio]')].at(-1).click());
  const otherInput=host().querySelector('input.rq-answer-text');assert.ok(otherInput,'selecting Other reveals a text input');
  await act(async()=>{const setter=Object.getOwnPropertyDescriptor(window.window.HTMLInputElement.prototype,'value').set;setter.call(otherInput,'Custom answer');otherInput.dispatchEvent(new window.window.Event('input',{bubbles:true}));});
  await act(async()=>button('Send answers').click());
  assert.equal(asks.length,2);assert.match(asks[1],/It never touches the base \(your suggested answer\)|It never touches the base/);assert.match(asks[1],/Custom answer/);assert.match(asks[1],/Do not run it and do not approve it/);
  assert.match(host().textContent,/went to the assistant/);
  // Draft whose question is open shows no Approve and points to the question.
  await render(React.createElement(m.RequirementCard,{info:drafted,actions}));
  assert.equal(button('Approve'),undefined);assert.match(host().textContent,/Waiting for your answer/);
  assert.equal(host().querySelector('.rq-badge').textContent,'Needs answer');

  // Draft with no print and duplicate build errors: no "can't measure it", errors deduplicated & formatted, badge is Needs fix.
  const buildErr={code:'E_BUILD',severity:'error',at:'claims[0]',message:"@parts/tensioner/crank_screw/v1/carriage_frame_assembly.py#carriage_frame_assembly failed to build: name 'import_stl' is not defined",fix:null};
  const failedDraft=info({text:'@[carriage.stl](parts/tensioner/crank_screw/v1/stl/carriage.stl#$file) must attach to @[Frame](parts/tensioner/crank_screw/v1/mount.py#frame)',print:null,rendered:null,diagnostics:[buildErr,{...buildErr,at:'claims[1]'},{...buildErr,at:'claims[2]'}]});
  await render(React.createElement(m.RequirementCard,{info:failedDraft,actions}));
  assert.equal(host().querySelector('.rq-badge').textContent,'Needs fix');
  assert.equal(host().querySelector('.rq-prose .unc'),null,'no false uncovered highlight when print is missing');
  assert.doesNotMatch(host().textContent,/can’t measure|won’t be checked/);
  assert.equal(host().querySelectorAll('.rq-callout.bad li').length,0,'single deduplicated error renders without a bullet list');
  assert.match(host().querySelector('.rq-callout.bad').textContent,/carriage_frame_assembly\.py failed to build: name 'import_stl' is not defined/);
  assert.doesNotMatch(host().querySelector('.rq-callout.bad').textContent,/parts\/tensioner/);
  assert.equal(host().querySelectorAll('#block-req-1').length,1,'error is not duplicated below the buttons');
  await act(async()=>root.unmount());
 }finally{try{await act(async()=>root.unmount());}catch{}console.error=error;window.window.close();await cleanup();}
});
