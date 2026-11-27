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
  const parts=[{model:models[0].model,modelLabel:'Cabinet',name:'door',detail:'cabinet.py'}];
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
  // Nothing is selected by default; only keyboard/hover picks a row.
  assert.equal(editor().getAttribute('aria-activedescendant'),null);
  assert.equal([...document.querySelectorAll('[role=option]')].some(item=>item.getAttribute('aria-selected')==='true'),false);

  // Arrows select the first row, then move (and wrap), Escape closes.
  await key('ArrowDown');
  assert.equal(document.querySelectorAll('[role=option]')[0].getAttribute('aria-selected'),'true');
  assert.equal(editor().getAttribute('aria-activedescendant'),document.querySelectorAll('[role=option]')[0].id);
  await key('ArrowDown');
  assert.equal(document.querySelectorAll('[role=option]')[1].getAttribute('aria-selected'),'true');
  await key('ArrowDown');
  assert.equal(document.querySelectorAll('[role=option]')[0].getAttribute('aria-selected'),'true');
  await key('Escape');
  assert.equal(document.querySelector('[role=listbox]'),null);

  // Enter picks the highlighted model as a chip, with a space after it.
  await type(' ');await type('@');
  assert.equal(options().length,3);
  await type('dra');
  assert.deepEqual(options(),['Drawer']);
  await key('ArrowDown');
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
  await key('ArrowDown');
  await key('Tab');
  assert.match(value,/@\[Cabinet\]\(cabinet\.py#cabinet\) $/);
  await type('@');
  await act(async()=>{[...document.querySelectorAll('[role=option]')].find(item=>item.textContent.startsWith('Door')).click();});
  assert.match(value,/@\[Door\]\(door\.py#door\) $/);

  // A published part is suggested as a bound model/part token.
  value='';await rerender({value:'',parts});
  await type('@door');
  assert.deepEqual(options(),['Door','Cabinet / door']);
  await act(async()=>[...document.querySelectorAll('[role=option]')].find(item=>item.textContent.startsWith('Cabinet / door')).click());
  const partChip=[...document.querySelectorAll('.req-chip')].at(-1);
  assert.equal(partChip.dataset.part,'door');
  assert.equal(partChip.textContent,'Cabinet / door');
  assert.ok(value.endsWith('@[Cabinet / door](cabinet.py#cabinet/door) '));

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

test('composer: Enter prepares while Shift+Enter adds a line',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=[{model:{entry:'door.py',builder:'door'},label:'Door',detail:'door.py'}];
 let value='',submits=0;
 const view=()=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,label:'What should the model do?',onSubmit:()=>{submits++;}});
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();const node=document.createTextNode(text);el.append(node);caret(node,text.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const key=async(name,init={})=>{let event;await act(async()=>{event=new window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...init});editor().dispatchEvent(event);});return event;};
 const error=console.error;console.error=()=>{};
 try{
  await act(async()=>root.render(view()));
  await type('Door must open');
  const enter=await key('Enter');
  assert.equal(enter.defaultPrevented,true);
  assert.equal(submits,1);
  assert.equal(value,'Door must open');
  await key('Enter',{shiftKey:true});
  assert.equal(submits,1);
  assert.equal(value,'Door must open\n');
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});

test('composer: open models list first, then best matches when filtering',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=['cabinet.py','door.py','drawer.py'].map(entry=>({model:{entry,builder:entry.slice(0,-3)},label:entry[0].toUpperCase()+entry.slice(1,-3),detail:entry}));
 let value='';
 const view=(props={})=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,openModels:[models[2].model],label:'What should the model do?',placeholder:'Describe it',...props});
 const rerender=async(props)=>act(async()=>root.render(view(props)));
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();let node=el.lastChild;if(!node||node.nodeType!==3){node=document.createTextNode('');el.append(node);}node.textContent+=text;caret(node,node.textContent.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const options=()=>[...document.querySelectorAll('[role=option]')].map(item=>item.querySelector('span').textContent);
 const error=console.error;console.error=()=>{};
 try{
  await rerender();
  // With no filter, the model already open in a tab comes first.
  await type('@');
  assert.deepEqual(options(),['Drawer','Cabinet','Door']);
  // Once filtering, the earlier match wins: Drawer's "r" (index 1) beats Door's (index 3).
  await type('r');
  assert.deepEqual(options(),['Drawer','Door']);
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});

test('mention ordering prefers open tabs, then best matches',async()=>{
 const {module:{orderMentions,filterMentions,scoreMention},cleanup}=await load('../src/mention-list.tsx');
 try{
  const choices=[{key:'trailing',label:'parts/trailing.py'},{key:'rail',label:'parts/rail.py'},{key:'open',label:'zeta.py',rank:0}];
  // No query: the open tab wins even though it is unrelated.
  assert.deepEqual(orderMentions(choices,'').map(item=>item.key),['open','trailing','rail']);
  // A query: a path-segment match beats an interior substring.
  assert.deepEqual(orderMentions(filterMentions(choices,'rail'),'rail').map(item=>item.key),['rail','trailing']);
  assert.ok(scoreMention(choices[1],'rail')<scoreMention(choices[0],'rail'));
  // Exact label beats a prefix, which beats an interior match.
  const exact=[{key:'exact',label:'door'},{key:'prefix',label:'door hinge'},{key:'inside',label:'woodoor'}];
  assert.deepEqual(orderMentions(exact,'door').map(item=>item.key),['exact','prefix','inside']);
  // Underscores/hyphens act as word breaks and words may be out of order.
  assert.deepEqual(filterMentions([{key:'frame',label:'v1_frame_name'}],'frame v1').map(item=>item.key),['frame']);
  assert.deepEqual(filterMentions([{key:'frame',label:'v1-frame-name'}],'name v1').map(item=>item.key),['frame']);
  // File path segments count as separate words.
  assert.deepEqual(filterMentions([{key:'rail',label:'parts/rail.py'}],'rail py').map(item=>item.key),['rail']);
  // Longer tokens tolerate a typo via subsequence matching.
  assert.deepEqual(filterMentions([{key:'frame',label:'v1_frame_name'}],'frme').map(item=>item.key),['frame']);
  // An unrelated word still excludes the entry.
  assert.deepEqual(filterMentions([{key:'frame',label:'v1_frame_name'}],'v2').map(item=>item.key),[]);
  // In-order whole-label match beats an out-of-order word match.
  const words=[{key:'unordered',label:'v1_frame_name'},{key:'ordered',label:'frame_v1_mount'}];
  assert.deepEqual(orderMentions(filterMentions(words,'frame v1'),'frame v1').map(item=>item.key),['ordered','unordered']);
 }finally{await cleanup();}
});

test('composer: spaces extend the mention query until it stops matching',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=[{model:{entry:'v1_frame_name.py',builder:'v1'},label:'v1_frame_name',detail:'v1_frame_name.py'}];
 let value='';
 const view=(props={})=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,label:'What should the model do?',placeholder:'Describe it',...props});
 const rerender=async(props)=>act(async()=>root.render(view(props)));
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();let node=el.lastChild;if(!node||node.nodeType!==3){node=document.createTextNode('');el.append(node);}node.textContent+=text;caret(node,node.textContent.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const options=()=>[...document.querySelectorAll('[role=option]')].map(item=>item.querySelector('span').textContent);
 const error=console.error;console.error=()=>{};
 try{
  await rerender();
  await type('@v1');
  assert.equal(editor().getAttribute('aria-expanded'),'true');
  await type(' ');
  // The space is part of the query and does not dismiss the picker.
  assert.equal(editor().getAttribute('aria-expanded'),'true');
  assert.deepEqual(options(),['v1_frame_name']);
  await type('fra');
  assert.equal(editor().getAttribute('aria-expanded'),'true');
  assert.deepEqual(options(),['v1_frame_name']);
  // A word that cannot match any model ends the mention instead of lingering.
  await type(' zzz');
  assert.equal(editor().getAttribute('aria-expanded'),'false');
  assert.equal(document.querySelector('[role=listbox]'),null);
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});

test('composer: Shift+Arrow selects several entries to insert at once',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=['cabinet.py','door.py','drawer.py'].map(entry=>({model:{entry,builder:entry.slice(0,-3)},label:entry[0].toUpperCase()+entry.slice(1,-3),detail:entry}));
 let value='';
 const view=()=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,label:'What should the model do?',placeholder:'Describe it'});
 const rerender=async()=>act(async()=>root.render(view()));
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();let node=el.lastChild;if(!node||node.nodeType!==3){node=document.createTextNode('');el.append(node);}node.textContent+=text;caret(node,node.textContent.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const key=async(name,init={})=>{let event;await act(async()=>{event=new window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...init});editor().dispatchEvent(event);});return event;};
 const selected=()=>[...document.querySelectorAll('[role=option]')].filter(item=>item.getAttribute('aria-selected')==='true').map(item=>item.querySelector('span').textContent);
 const error=console.error;console.error=()=>{};
 try{
  await rerender();
  await type('@');
  // Opening the picker selects nothing; a plain arrow selects exactly one row.
  assert.deepEqual(selected(),[]);
  await key('ArrowDown');
  assert.deepEqual(selected(),['Cabinet']);
  await key('ArrowDown',{shiftKey:true});
  assert.deepEqual(selected(),['Cabinet','Door']);
  await key('ArrowDown',{shiftKey:true});
  assert.deepEqual(selected(),['Cabinet','Door','Drawer']);
  await key('Enter');
  assert.equal(document.querySelector('[role=listbox]'),null);
  assert.deepEqual([...document.querySelectorAll('.req-chip')].map(chip=>chip.textContent),['Cabinet','Door','Drawer']);
  assert.match(value,/\@\[Cabinet\]\(cabinet\.py#cabinet\) @\[Door\]\(door\.py#door\) @\[Drawer\]\(drawer\.py#drawer\) $/);
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});

test('composer: STL and STEP models of one part share a suggestion row',async()=>{
 const {module:{RequirementComposer},cleanup}=await load('../src/requirement-composer.tsx');
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','HTMLBRElement','Node','Event','KeyboardEvent','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const root=createRoot(document.getElementById('root'));
 const models=[
  {model:{entry:'parts/bracket.stl',builder:'$file'},label:'bracket.stl',detail:'parts/bracket.stl'},
  {model:{entry:'parts/bracket.step',builder:'$file'},label:'bracket.step',detail:'parts/bracket.step'},
 ];
 let value='';
 const view=()=>React.createElement(RequirementComposer,{value,onChange:next=>{value=next;},models,label:'What should the model do?',placeholder:'Describe it'});
 const rerender=async()=>act(async()=>root.render(view()));
 const editor=()=>document.querySelector('[role=textbox]');
 const caret=(node,offset)=>{const range=document.createRange();range.setStart(node,offset);range.collapse(true);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);};
 const type=async text=>act(async()=>{const el=editor();let node=el.lastChild;if(!node||node.nodeType!==3){node=document.createTextNode('');el.append(node);}node.textContent+=text;caret(node,node.textContent.length);el.dispatchEvent(new window.Event('input',{bubbles:true}));});
 const key=async(name,init={})=>{let event;await act(async()=>{event=new window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...init});editor().dispatchEvent(event);});return event;};
 const error=console.error;console.error=()=>{};
 try{
  await rerender();
  await type('@');
  const options=[...document.querySelectorAll('[role=option]')];
  assert.equal(options.length,1,'STL and STEP collapse into one suggestion');
  assert.equal(options[0].querySelector('span').textContent,'parts/bracket');
  assert.equal(options[0].querySelector('small').textContent,'STL + STEP');
  await key('ArrowDown');
  await key('Enter');
  // The requirement still targets one model: exact STEP wins over mesh STL.
  assert.equal(value,'@[bracket.step](parts/bracket.step#$file) ');
  assert.deepEqual([...document.querySelectorAll('.req-chip')].map(chip=>chip.textContent),['bracket.step']);
 }finally{
  console.error=error;
  await act(async()=>root.unmount());dom.window.close();await cleanup();
 }
});
