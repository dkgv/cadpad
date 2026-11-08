// CPU DOM regressions; no browser, native app, WebGL or user storage.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,writeFile,rm} from 'node:fs/promises';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import vm from 'node:vm';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import React,{act} from 'react';
const bootstrap=await readFile(new URL('../public/startup.js',import.meta.url),'utf8');
test('module-load failures keep a visible reload screen and escape error text',()=>{
 const dom=new JSDOM('<div id="root"><main>Opening your workspace…</main></div>',{url:'https://tauri.localhost',runScripts:'outside-only'});
 vm.runInContext(bootstrap,dom.getInternalVMContext());
 const script=dom.window.document.createElement('script');script.src='/assets/missing.js';dom.window.document.body.append(script);script.dispatchEvent(new dom.window.Event('error'));
 assert.match(dom.window.document.getElementById('root').textContent,/Cadpad couldn’t start/);
 assert.match(dom.window.document.querySelector('pre').textContent,/missing.js/);
 dom.window.cadpadStartup.fail('<img src=x onerror=alert(1)>');
 assert.equal(dom.window.document.querySelector('img'),null);
 assert.match(dom.window.document.querySelector('pre').textContent,/<img/);
 dom.window.close();
});
test('global startup handler leaves an already mounted workspace intact',()=>{
 const dom=new JSDOM('<div id="root">Workspace</div>',{url:'https://tauri.localhost',runScripts:'outside-only'});
 vm.runInContext(bootstrap,dom.getInternalVMContext());dom.window.cadpadStartup.ready();
 dom.window.cadpadStartup.fail(new Error('Later background failure'));
 assert.equal(dom.window.document.getElementById('root').textContent,'Workspace');dom.window.close();
});
test('render failure shows recovery actions and new scratchpad uses the native command without deleting projects',async()=>{
 const directory=await mkdtemp(path.join(process.cwd(),'target/startup-unit-'));
 const result=await build({entryPoints:[new URL('../src/startup.tsx',import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,external:['react'],plugins:[{name:'native-fixture',setup(builder){builder.onResolve({filter:/^@tauri-apps\/api\/core$/},()=>({path:'native',namespace:'fixture'}));builder.onLoad({filter:/.*/,namespace:'fixture'},()=>({contents:'export const invoke=(...args)=>globalThis.__startupInvoke(...args);',loader:'js'}));}}]});
 const file=path.join(directory,'boundary.mjs');await writeFile(file,result.outputFiles[0].text);
 const dom=new JSDOM('<div id="root"></div>',{url:'https://tauri.localhost'});
 for(const key of ['window','document','navigator','HTMLElement','Node','Event','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const calls=[];globalThis.__startupInvoke=async command=>{calls.push(command);throw new Error('Synthetic command failure');};
 const {createRoot}=await import('react-dom/client');const {StartupBoundary}=await import(pathToFileURL(file));const root=createRoot(document.getElementById('root'));
 const error=console.error;console.error=()=>{};
 try{
  function Broken(){throw new Error('Invalid workspace render');}
  await act(async()=>root.render(React.createElement(StartupBoundary,null,React.createElement(Broken))));
  assert.match(document.body.textContent,/Cadpad couldn’t open this workspace/);
  assert.match(document.querySelector('pre').textContent,/Invalid workspace render/);
  const button=[...document.querySelectorAll('button')].find(button=>button.textContent==='New scratchpad');
  await act(async()=>{button.click();await new Promise(resolve=>setTimeout(resolve,5));});
  assert.deepEqual(calls,['new_scratchpad']);assert.match(document.body.textContent,/Synthetic command failure/);
 }finally{await act(async()=>root.unmount());console.error=error;dom.window.close();await rm(directory,{recursive:true,force:true});delete globalThis.__startupInvoke;}
});

test('failure before the root is parsed becomes visible when the document is ready',()=>{
 const dom=new JSDOM('<head></head><body></body>',{url:'https://tauri.localhost',runScripts:'outside-only'});
 vm.runInContext(bootstrap,dom.getInternalVMContext());dom.window.cadpadStartup.fail('Early module error');
 const root=dom.window.document.createElement('div');root.id='root';dom.window.document.body.append(root);
 dom.window.document.dispatchEvent(new dom.window.Event('DOMContentLoaded'));
 assert.match(root.textContent,/Early module error/);dom.window.close();
});
