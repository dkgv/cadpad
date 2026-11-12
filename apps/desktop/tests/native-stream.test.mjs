import test from 'node:test';
import assert from 'node:assert/strict';
import {build} from 'esbuild';

const bundle=await build({entryPoints:[new URL('../src/native-stream.ts',import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,plugins:[{name:'native-fixture',setup(builder){
  builder.onResolve({filter:/^@tauri-apps\/api\/core$/},()=>({path:'native',namespace:'fixture'}));
  builder.onLoad({filter:/.*/,namespace:'fixture'},()=>({contents:'export const invoke=(...args)=>globalThis.__streamInvoke(...args);export class Channel{id=++globalThis.__streamChannels;onmessage=()=>{};}',loader:'js'}));
}}]});
const {connectNativeStream,messageChannel}=await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`);

test('closing a stream detaches the session immediately and waits for registration before disconnecting',async()=>{
  let finish,received=0;const calls=[];
  globalThis.__streamInvoke=(command,args)=>{calls.push([command,args]);return command==='connect_activity'?new Promise(resolve=>{finish=resolve;}):Promise.resolve();};
  const channel={id:42,onmessage:()=>received++};
  const close=connectNativeStream('connect_activity',channel,()=>assert.fail('unexpected registration failure'));
  close();channel.onmessage({kind:'refresh'});
  assert.equal(received,0,'a late native event cannot revive a closed React session');
  assert.deepEqual(calls.map(([command])=>command),['connect_activity']);
  finish();await new Promise(resolve=>setImmediate(resolve));
  assert.deepEqual(calls.at(-1),['disconnect_stream',{subscriptionId:42}]);
  delete globalThis.__streamInvoke;
});

test('local dock routing allocates no persistent native callbacks',()=>{
  globalThis.__streamChannels=0;
  for(let i=0;i<100;i++){const channel=messageChannel(true);channel.onmessage=()=>{};}
  assert.equal(globalThis.__streamChannels,0);
  assert.equal(messageChannel(false).id,1);
  delete globalThis.__streamChannels;
});
