// CPU-only production-entry smoke test. No browser, native app, WebGL or user data.
import jsdom from 'jsdom';
const {JSDOM,requestInterceptor,VirtualConsole}=jsdom;
import {readFile} from 'node:fs/promises';
import path from 'node:path';
import vm from 'node:vm';
import {pathToFileURL,fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=path.resolve(process.argv[2]||'apps/desktop/dist');
const html=await readFile(path.join(root,'index.html'),'utf8');
const project=JSON.parse(await readFile(path.join(root,'startup-project.json'),'utf8'));
const resources={interceptors:[requestInterceptor(async request=>{
  const parsed=new URL(request.url);const file=parsed.protocol==='file:'?fileURLToPath(parsed):path.join(root,parsed.pathname);
  if(!path.resolve(file).startsWith(root+path.sep))throw new Error('Unexpected resource '+request.url);
  return new Response(await readFile(file),{headers:{'Content-Type':file.endsWith('.css')?'text/css':'application/javascript'}});
})]};
const console=new VirtualConsole();
const failures=[];console.on('jsdomError',error=>{if(!error.message.includes('getContext')&&!error.message.includes('Could not parse CSS stylesheet'))failures.push(error.message);});
const dom=new JSDOM(html,{url:'https://tauri.localhost',pretendToBeVisual:true,runScripts:'outside-only',resources,virtualConsole:console});
for(const key of ['window','document','navigator','HTMLElement','HTMLScriptElement','Element','HTMLCanvasElement','HTMLDialogElement','Node','MutationObserver','getComputedStyle'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
globalThis.requestAnimationFrame=dom.window.requestAnimationFrame.bind(dom.window);globalThis.cancelAnimationFrame=dom.window.cancelAnimationFrame.bind(dom.window);
globalThis.fetch=async input=>{
 const parsed=new URL(typeof input==='string'?input:input.url||String(input),window.location.href);
 const file=parsed.protocol==='file:'?fileURLToPath(parsed):path.join(root,parsed.pathname);
 if(!path.resolve(file).startsWith(root+path.sep))throw new Error('Unexpected fetch '+parsed.href);
 return new Response(await readFile(file));
};
const calls=[];
window.__TAURI_INTERNALS__={transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(command)=>{
 calls.push(command);
 if(command==='initialize'||command==='project')return structuredClone(project);
 if(command==='workspaces')return [{workspace_id:project.workspace_id,name:project.name,running:0}];
 if(command==='agent_conversation')return {messages:[],turn:null};
 if(command==='agent_settings')return {provider:'fixture',model:'fixture',configured:false,credential_storage:'fixture'};
 if(command==='mcp_settings')return {config:{},bridge_installed:true,sessions:[]};
 if(command==='motion_tests')return {catalog:'fixture',tests:[],accuracy:'Fixture',snapshot:null};
 return null;
}};
try{
 vm.runInContext(await readFile(path.join(root,'startup.js'),'utf8'),dom.getInternalVMContext());
 assert.match(document.getElementById('root').textContent,/Opening your workspace/);
 const src=html.match(/<script[^>]*type="module"[^>]*src="([^"]+)"/)[1];
 await import(pathToFileURL(path.join(root,src.replace(/^\//,''))).href);
 const deadline=Date.now()+5000;
 while(!document.querySelector('.app')&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,20));
 assert.ok(document.querySelector('.app'),'Production entry did not render the workspace: '+document.getElementById('root').textContent);
 while(!calls.includes('agent_conversation')&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,20));
 await new Promise(resolve=>setTimeout(resolve,100));
 assert.ok(document.querySelector('.app'),'Workspace disappeared after native startup replies');
 assert.ok(calls.includes('initialize')&&calls.includes('agent_conversation'));
 assert.equal(failures.length,0,failures.join('\n'));
 process.stdout.write(JSON.stringify({status:'passed',production_entry:src,embedded_assets:true,real_native_startup_reply:true,viewer:'actual dependency, expected CPU-only graphics fallback',native_transport:'mocked',ui_launched:false})+'\n');
 dom.window.close();process.exit(0);
}catch(error){process.stderr.write(error.stack+'\n');dom.window.close();process.exit(1);}
