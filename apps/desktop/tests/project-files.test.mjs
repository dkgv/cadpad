import test from 'node:test';
import assert from 'node:assert/strict';
import {build} from 'esbuild';

const bundle=await build({entryPoints:[new URL('../src/project-files.ts',import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,plugins:[{name:'native-fixture',setup(builder){
  builder.onResolve({filter:/^@tauri-apps\/api\/core$/},()=>({path:'native',namespace:'fixture'}));
  builder.onLoad({filter:/.*/,namespace:'fixture'},()=>({contents:'export const invoke=(...args)=>globalThis.__fileInvoke(...args);export class Channel{id=++globalThis.__fileChannels;onmessage=()=>{};}',loader:'js'}));
}}]});
const {watchProjectFiles,reconnectProjectFiles}=await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`);
const flush=()=>new Promise(resolve=>setImmediate(resolve));

test('project observers share a native stream, refresh on focus, and detach before pending registration finishes',async()=>{
  globalThis.window=new EventTarget();globalThis.__fileChannels=0;
  let finish;const calls=[];
  globalThis.__fileInvoke=(command,args)=>{calls.push([command,args]);return command==='connect_project_files'?new Promise(resolve=>{finish=resolve;}):Promise.resolve();};
  let models=0,files=0;
  const stopModels=watchProjectFiles('project',()=>models++);
  const stopFiles=watchProjectFiles('project',()=>files++);
  assert.equal(calls.length,1);
  const channel=calls[0][1].channel;
  assert.equal(calls[0][1].workspace,'project');
  channel.onmessage({kind:'ready'});channel.onmessage({kind:'changed'});
  assert.deepEqual([models,files],[2,2]);
  window.dispatchEvent(new Event('focus'));
  assert.deepEqual([models,files],[3,3]);
  stopModels();channel.onmessage({kind:'changed'});
  assert.deepEqual([models,files],[3,4]);
  assert.equal(calls.length,1,'one remaining observer keeps the watch alive');
  stopFiles();channel.onmessage({kind:'changed'});window.dispatchEvent(new Event('focus'));
  assert.deepEqual([models,files],[3,4],'late callbacks cannot revive a closed project');
  finish();await flush();
  assert.deepEqual(calls.at(-1),['disconnect_stream',{subscriptionId:channel.id}]);
  delete globalThis.window;delete globalThis.__fileInvoke;delete globalThis.__fileChannels;
});

test('failed watcher registration is reported and retried on focus without polling',async()=>{
  globalThis.window=new EventTarget();globalThis.__fileChannels=0;
  const calls=[],errors=[];let changes=0,fail=true;
  globalThis.__fileInvoke=async(command,args)=>{calls.push([command,args]);if(command==='connect_project_files'&&fail)throw new Error('watch unavailable');};
  const stop=watchProjectFiles('retry',()=>changes++,error=>errors.push(error.message));
  await flush();
  assert.deepEqual(errors,['watch unavailable']);
  const oldChannel=calls[0][1].channel;
  fail=false;window.dispatchEvent(new Event('focus'));await flush();
  const registrations=calls.filter(([command])=>command==='connect_project_files');
  assert.equal(registrations.length,2);
  const channel=registrations[1][1].channel;
  oldChannel.onmessage({kind:'changed'});
  assert.equal(changes,1,'the failed stream is detached before retry');
  channel.onmessage({kind:'ready'});
  assert.equal(changes,2,'ready refreshes disk after installing the watcher');
  stop();await flush();
  assert.deepEqual(calls.at(-1),['disconnect_stream',{subscriptionId:channel.id}]);
  delete globalThis.window;delete globalThis.__fileInvoke;delete globalThis.__fileChannels;
});


test('saving to a different folder replaces the stream and preserves its observers',async()=>{
  globalThis.window=new EventTarget();globalThis.__fileChannels=0;
  const calls=[];let changes=0;
  globalThis.__fileInvoke=async(command,args)=>{calls.push([command,args]);};
  const stop=watchProjectFiles('save-as',()=>changes++);
  const oldChannel=calls[0][1].channel;
  reconnectProjectFiles('save-as');await flush();
  const registrations=calls.filter(([command])=>command==='connect_project_files');
  assert.equal(registrations.length,2);
  oldChannel.onmessage({kind:'changed'});
  assert.equal(changes,0);
  registrations[1][1].channel.onmessage({kind:'ready'});
  assert.equal(changes,1);
  stop();await flush();
  delete globalThis.window;delete globalThis.__fileInvoke;delete globalThis.__fileChannels;
});
