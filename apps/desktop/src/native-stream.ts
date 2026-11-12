import type {MessageSink} from './session-host';
import {invoke,Channel} from '@tauri-apps/api/core';

/** Detach callbacks immediately, then drop the Rust channel after registration completes. */
export function connectNativeStream<T>(command:string,channel:Channel<T>,onError:(error:unknown)=>void){
  let active=true;
  const connected=invoke<void>(command,{channel}).catch(error=>{if(active)onError(error);});
  return ()=>{
    active=false;
    channel.onmessage=()=>{};
    void connected.then(()=>invoke('disconnect_stream',{subscriptionId:channel.id})).catch(()=>{});
  };
}

/** Docked sessions route locally; only the shared workbench bridge allocates an IPC callback. */
export function messageChannel<T>(local:boolean):MessageSink<T>|Channel<T>{
  return local?{onmessage:()=>{}}:new Channel<T>();
}
