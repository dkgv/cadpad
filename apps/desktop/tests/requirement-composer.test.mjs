// CPU DOM tests for the `@` chip composer and its token helpers; no browser or native app.
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
 const directory=await mkdtemp(path.join(process.cwd(),'target/composer-unit-'));
 const result=await build({entryPoints:[new URL(entry,import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,external:['react','react-dom']});
 const file=path.join(directory,'module.mjs');await writeFile(file,result.outputFiles[0].text);
 return {module:await import(pathToFileURL(file)),cleanup:()=>rm(directory,{recursive:true,force:true})};
}

test('token helpers parse, serialize and list mentions',async()=>{
 const {module:{parseTokens,serializeTokens,mentionedModels,tokenFor},cleanup}=await load('../src/requirement-tokens.ts');
 try{
  const text='Move @[Door](cabinet.py#cabinet) and @[Hinge](cabinet.py#cabinet/hinge) with @[Door](cabinet.py#cabinet) shut';
  const segments=parseTokens(text);
  assert.deepEqual(segments.map(item=>item.kind),['text','token','text','token','text','token','text']);
  assert.deepEqual({...segments[3],raw:undefined},{kind:'token',label:'Hinge',entry:'cabinet.py',builder:'cabinet',part:'hinge',raw:undefined});
  assert.equal(serializeTokens(segments),text);
  // Distinct models in order; a part counts as its model.
  assert.deepEqual(mentionedModels(text),[{entry:'cabinet.py',builder:'cabinet'}]);
  assert.deepEqual(mentionedModels('@[A](a.py#a) then @[B](dir/b.py#b)'),[{entry:'a.py',builder:'a'},{entry:'dir/b.py',builder:'b'}]);
  // Renaming the label changes only the display text.
  assert.equal(serializeTokens(segments.map(item=>item.kind==='token'?{...item,label:'Renamed'}:item)).includes('@[Renamed](cabinet.py#cabinet)'),true);
  // Awkward paths round-trip.
  const odd={label:'A [x]',entry:'we(ird) 100%/m.py',builder:'b#1'};
  const parsed=parseTokens(tokenFor(odd));
  assert.equal(parsed.length,1);assert.equal(parsed[0].entry,odd.entry);assert.equal(parsed[0].builder,odd.builder);assert.equal(parsed[0].label,'A x');
  // Malformed tokens stay plain text; plain strings are unchanged.
  for(const plain of ['no tokens here','@[x](nohash)','@[x]() and @name','@[x](#b)','email me@[a]'])assert.deepEqual(mentionedModels(plain),[]);
  assert.deepEqual(parseTokens('@[x](nohash)'),[{kind:'text',text:'@[x](nohash)'}]);
  assert.deepEqual(parseTokens(''),[]);
 }finally{await cleanup();}
});

test('composer: picker, chips, keyboard, paste, stale chips',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=[
  {model:{entry:'cabinet.py',builder:'cabinet'},label:'Cabinet',detail:'cabinet.py'},
  {model:{entry:'door.py',builder:'door'},label:'Door',detail:'door.py'},
  {model:{entry:'drawer.py',builder:'drawer'},label:'Drawer',detail:'drawer.py'},
 ];
 let value='';
 const view=(props={})=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,label:'What should the model do?',placeholder:'Describe it',...props});
 const rerender=async(props)=>act(async()=>root.render(view(props)));
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();let node=el.lastChild;if(!node||node.nodeType!==3){node=document.createTextNode('');el.append(node);}node.textContent+=text;caret(node,node.textContent.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const key=async(name,init={})=>{let event;await act(async()=>{event=new window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...init});editor().dispatchEvent(event);});return event;};
 const options=()=>[...document.querySelectorAll('[role=option]')].map(item=>item.querySelector('span').textContent);
 const error=console.error;console.error=()=>{};
 try{
  await rerender();
  assert.ok(editor().getAttribute('aria-labelledby'));
  assert.equal(document.getElementById(editor().getAttribute('aria-labelledby')).textContent,'What should the model do?');
  assert.equal(editor().hasAttribute('data-empty'),true);
  assert.equal(document.querySelector('[role=listbox]'),null);

  // Typing @ lists every model; typing narrows by label.
  await type('Check the @');
  assert.deepEqual(options(),['Cabinet','Door','Drawer']);
  assert.equal(editor().getAttribute('aria-expanded'),'true');
  assert.equal(editor().getAttribute('aria-controls'),document.querySelector('[role=listbox]').id);
  await type('d');
  assert.deepEqual(options(),['Door','Drawer']);
  assert.equal(editor().getAttribute('aria-activedescendant'),document.querySelector('[role=option]').id);
  assert.equal(document.querySelector('[role=option]').getAttribute('aria-selected'),'true');

  // Arrows move (and wrap), Escape closes.
  await key('ArrowDown');
  assert.equal(document.querySelectorAll('[role=option]')[1].getAttribute('aria-selected'),'true');
  assert.equal(editor().getAttribute('aria-activedescendant'),document.querySelectorAll('[role=option]')[1].id);
  await key('ArrowDown');
  assert.equal(document.querySelectorAll('[role=option]')[0].getAttribute('aria-selected'),'true');
  await key('Escape');
  assert.equal(document.querySelector('[role=listbox]'),null);

  // Enter picks the highlighted model as a chip, with a space after it.
  await type(' ');await type('@');
  assert.equal(options().length,3);
  await type('dra');
  assert.deepEqual(options(),['Drawer']);
  await key('Enter');
  assert.equal(document.querySelector('[role=listbox]'),null);
  assert.equal(document.querySelectorAll('.req-chip').length,1);
  const chip=document.querySelector('.req-chip');
  assert.equal(chip.textContent,'Drawer');assert.equal(chip.getAttribute('contenteditable'),'false');assert.equal(chip.classList.contains('unresolved'),false);
  assert.equal(value,'Check the @d @[Drawer](drawer.py#drawer) ');

  // Backspace removes the chip as one unit.
  caret(chip.nextSibling,0);
  const event=await key('Backspace');
  assert.equal(event.defaultPrevented,true);
  assert.equal(document.querySelectorAll('.req-chip').length,0);
  assert.equal(value,'Check the @d  ');

  // Tab selects, and a click selects.
  await type('@cab');
  await key('Tab');
  assert.match(value,/@\[Cabinet\]\(cabinet\.py#cabinet\) $/);
  await type('@');
  await act(async()=>{[...document.querySelectorAll('[role=option]')].find(item=>item.textContent.startsWith('Door')).click();});
  assert.match(value,/@\[Door\]\(door\.py#door\) $/);

  // Paste keeps tokens, makes chips, and flags a model that left the project.
  const before=document.querySelectorAll('.req-chip').length;
  await act(async()=>{
   const el=editor();caret(el.lastChild,el.lastChild.textContent.length);
   const paste=new window.Event('paste',{bubbles:true,cancelable:true});
   paste.clipboardData={getData:()=>'then @[Drawer](drawer.py#drawer)\nand @[Gone](gone.py#gone)'};
   el.dispatchEvent(paste);
  });
  const chips=[...document.querySelectorAll('.req-chip')];
  assert.equal(chips.length,before+2);
  const stale=chips.at(-1);
  assert.equal(stale.classList.contains('unresolved'),true);assert.match(stale.getAttribute('aria-label'),/Missing model Gone/);
  assert.match(value,/then @\[Drawer\]\(drawer\.py#drawer\)\nand @\[Gone\]\(gone\.py#gone\)$/);

  // An outside reset rebuilds the editor from the string.
  value='@[Door](door.py#door) opens';
  await rerender();
  assert.equal(editor().textContent,'Door opens');
  assert.equal(document.querySelectorAll('.req-chip').length,1);
  value='';await rerender();
  assert.equal(editor().textContent,'');assert.equal(editor().hasAttribute('data-empty'),true);

  // Chips follow the project: a renamed label is shown, a removed model turns into a warning.
  value='@[Old name](door.py#door)';await rerender();
  assert.equal(document.querySelector('.req-chip').textContent,'Door');
  await rerender({models:[models[0]]});
  assert.equal(document.querySelector('.req-chip').classList.contains('unresolved'),true);
  assert.equal(value,'@[Door](door.py#door)');
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});
