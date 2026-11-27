// CPU tests for the Assistant file/folder mention rows; no browser or native app.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import React,{act} from 'react';

async function load(entry){
 await mkdir(path.join(process.cwd(),'target'),{recursive:true});
 const directory=await mkdtemp(path.join(process.cwd(),'target/mention-unit-'));
 const result=await build({entryPoints:[new URL(entry,import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,external:['react','react-dom']});
 const file=path.join(directory,'module.mjs');await writeFile(file,result.outputFiles[0].text);
 return {module:await import(pathToFileURL(file)),cleanup:()=>rm(directory,{recursive:true,force:true})};
}

test('project mention files group STL and STEP exports of one part',async()=>{
 const {module:{projectFileMentions},cleanup}=await load('../src/mention-textarea.tsx');
 try{
  const items=projectFileMentions(['parts/rail.stl','parts/rail.step','parts/rail.py','parts/carriage.stl']);
  // One row for the rail part, carrying both export paths.
  const rail=items.find(item=>item.label==='parts/rail');
  assert.ok(rail,'STL and STEP of the same part collapse into one row');
  assert.deepEqual(rail.paths,['parts/rail.stl','parts/rail.step']);
  assert.equal(rail.path,'parts/rail.stl');
  assert.equal(rail.detail,'STL + STEP');
  // A lone STL stays a normal path row.
  const carriage=items.find(item=>item.path==='parts/carriage.stl');
  assert.equal(carriage.paths,undefined);
  assert.equal(carriage.label,undefined);
  // Source files are untouched and the grouped part replaces both export rows.
  assert.ok(items.some(item=>item.path==='parts/rail.py'&&item.paths===undefined));
  assert.equal(items.filter(item=>item.path.startsWith('parts/rail.')).length,2);
  assert.equal(items.some(item=>item.path==='parts/rail.step'),false);
 }finally{await cleanup();}
});

test('chat mention picker auto-selects the first entry and ArrowDown moves to the second',async()=>{
 const {module:{MentionTextarea},cleanup}=await load('../src/mention-textarea.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 dom.window.HTMLElement.prototype.scrollIntoView=()=>{};
 for(const key of ['window','document','navigator','HTMLElement','HTMLTextAreaElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.requestAnimationFrame=cb=>setTimeout(cb,0);
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const items=['cabinet.py','door.py','drawer.py'].map(path=>({kind:'file',path}));
 let latest='';
 function Harness(){
  const [value,setValue]=React.useState('');
  latest=value;
  return React.createElement(MentionTextarea,{value,onValueChange:setValue,items,'aria-label':'Describe your model'});
 }
 const textarea=()=>document.querySelector('textarea');
 const options=()=>[...document.querySelectorAll('[role=option]')];
 const selected=()=>options().filter(item=>item.getAttribute('aria-selected')==='true').map(item=>item.querySelector('span').textContent);
 const type=async text=>act(async()=>{
  const el=textarea();
  const next=el.value+text;
  const proto=dom.window.HTMLTextAreaElement.prototype;
  Object.getOwnPropertyDescriptor(proto,'value').set.call(el,next);
  el.setSelectionRange(next.length,next.length);
  el.dispatchEvent(new dom.window.Event('input',{bubbles:true}));
 });
 const flush=async()=>act(async()=>new Promise(resolve=>setTimeout(resolve,20)));
 const key=async(name,init={})=>{
  let event;
  await act(async()=>{
   event=new dom.window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...init});
   textarea().dispatchEvent(event);
  });
  return event;
 };
 const error=console.error;console.error=()=>{};
 try{
  await act(async()=>{root.render(React.createElement(Harness));});
  await type('@');
  assert.deepEqual(options().map(item=>item.querySelector('span').textContent),['cabinet.py','door.py','drawer.py']);
  // First entry is auto-selected when the picker opens.
  assert.deepEqual(selected(),['cabinet.py']);
  assert.equal(textarea().getAttribute('aria-activedescendant'),options()[0].id);

  // ArrowDown moves to the second entry, not the first.
  await key('ArrowDown');
  assert.deepEqual(selected(),['door.py']);
  assert.equal(textarea().getAttribute('aria-activedescendant'),options()[1].id);

  // Enter inserts the currently selected (second) entry.
  await key('Enter');
  await flush();
  assert.equal(latest,'@door.py ');

  // Opening again and pressing Enter immediately inserts the auto-selected first entry.
  await type('@');
  assert.deepEqual(selected(),['cabinet.py']);
  await key('Enter');
  await flush();
  assert.equal(latest,'@door.py @cabinet.py ');

  // Shift+ArrowDown extends from the auto-selected first entry to include the second.
  await type('@');
  assert.deepEqual(selected(),['cabinet.py']);
  await key('ArrowDown',{shiftKey:true});
  assert.deepEqual(selected(),['cabinet.py','door.py']);
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});
