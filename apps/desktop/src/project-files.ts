import {Channel} from '@tauri-apps/api/core';
import {connectNativeStream} from './native-stream';

type Observer={change:()=>void;error:(error:unknown)=>void};
type ProjectWatch={observers:Set<Observer>;close:()=>void;reconnect:()=>void};
const watches=new Map<string,ProjectWatch>();

/** Share one native stream per workspace across Explorer, review, and model views. */
export function watchProjectFiles(workspace:string,onChange:()=>void,onError:(error:unknown)=>void=()=>{}){
  const observer={change:onChange,error:onError};
  let watch=watches.get(workspace);
  if(!watch){
    const observers=new Set<Observer>([observer]);
    let failed=false,active=true;
    const emit=()=>{for(const item of observers)item.change();};
    const connect=()=>{
      const channel=new Channel<{kind:'ready'|'changed'}>();
      channel.onmessage=()=>{if(active)emit();};
      failed=false;
      return connectNativeStream('connect_project_files',channel,error=>{
        failed=true;for(const item of observers)item.error(error);
      },{workspace});
    };
    let disconnect=connect();
    const reconnect=()=>{disconnect();disconnect=connect();};
    const focus=()=>{
      if(failed)reconnect();
      emit();
    };
    window.addEventListener('focus',focus);
    watch={observers,reconnect,close:()=>{active=false;window.removeEventListener('focus',focus);disconnect();}};
    watches.set(workspace,watch);
  }else watch.observers.add(observer);
  const current=watch;
  return()=>{
    current.observers.delete(observer);
    if(!current.observers.size){current.close();if(watches.get(workspace)===current)watches.delete(workspace);}
  };
}

/** A successful Save / Save As can bind the same workspace to a different folder. */
export function reconnectProjectFiles(workspace:string){watches.get(workspace)?.reconnect();}
