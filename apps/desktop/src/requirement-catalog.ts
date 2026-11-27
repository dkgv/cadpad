import {useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {reviewHashes,type ProsePrintAdvisory,type RequirementInfo,type RequirementRun,type RequirementsReply} from './requirement-review';

const errorText=(value:unknown)=>value instanceof Error?value.message:typeof value==='object'&&value&&'message' in value?String((value as {message:unknown}).message):String(value);
const activeState=(run?:RequirementRun|null)=>!!run&&['queued','running','cancelling'].includes(run.state);
type AdvisoryTask={attempts:number;promise?:Promise<ProsePrintAdvisory>;result?:ProsePrintAdvisory};
const advisoryTasks=new Map<string,AdvisoryTask>();
const sameHashes=(left:RequirementInfo['draft_hashes'],right:RequirementInfo['draft_hashes'])=>!!left&&!!right&&left.requirement===right.requirement&&left.manifest===right.manifest&&left.plan===right.plan;

async function requestAdvisory(workspace:string,info:RequirementInfo):Promise<ProsePrintAdvisory>{
  if(!info.draft_hashes)throw new Error('A successful saved compile is required before cross-checking.');
  const hashes=info.draft_hashes;
  const key=JSON.stringify([workspace,info.id,info.draft_hash,hashes,info.rendered]);
  let task=advisoryTasks.get(key);
  if(!task){task={attempts:0};advisoryTasks.set(key,task);}
  if(task.result)return task.result;
  if(task.promise)return task.promise;
  if(task.attempts>=3)throw new Error('The advisory cross-check failed three times for this exact draft. Edit or resave the draft before trying again.');
  task.attempts++;
  const promise=invoke<ProsePrintAdvisory>('cross_check_requirement',{
    workspace,id:info.id,draftHash:info.draft_hash,draftHashes:hashes,
  }).then(result=>{
    if(result.draft_hash!==info.draft_hash||!sameHashes(result.draft_hashes,hashes))throw new Error('The saved draft changed while the cross-check was running. Refresh the review.');
    // Successes are stable for this exact saved print. Unavailable responses
    // remain retryable, subject to the same bounded attempt counter as errors.
    if(result.available)task!.result=result;
    return result;
  }).finally(()=>{task!.promise=undefined;});
  task.promise=promise;
  if(advisoryTasks.size>128){const oldest=advisoryTasks.keys().next().value;if(oldest&&oldest!==key)advisoryTasks.delete(oldest);}
  return promise;
}

/**
 * The project's requirement catalog: polls `requirements` list (quickly while something is queued or running,
 * otherwise every 2 s) and carries the user-only commands.
 */
export function useRequirementCatalog(project?:string){
  const [reply,setReply]=useState<RequirementsReply|null>(null),[error,setError]=useState(''),[busy,setBusy]=useState<string|null>(null);
  const epoch=useRef(0),order=useRef(0),pending=useRef(false),lastRead=useRef(0),running=useRef(false),current=useRef<RequirementsReply|null>(null);
  function adopt(next:RequirementsReply,token:number){
    if(token!==epoch.current||!next||!Array.isArray(next.requirements))return;
    current.current=next;setReply(next);
    running.current=activeState(next.run);
  }
  const call=(command:'requirements'|'approve_requirement'|'delete_requirement',request:Record<string,unknown>)=>invoke<RequirementsReply>(command,{request:{workspace_id:project,...request}});
  useEffect(()=>{
    const token=++epoch.current;
    setReply(null);setError('');setBusy(null);current.current=null;running.current=false;lastRead.current=0;pending.current=false;
    if(!project)return()=>{epoch.current++;};
    let active=true,reading=false;
    const refresh=()=>{
      if(reading||pending.current||(!running.current&&lastRead.current&&performance.now()-lastRead.current<2000))return;
      reading=true;lastRead.current=performance.now();const mine=++order.current;
      invoke<RequirementsReply>('requirements',{request:{workspace_id:project,expected_catalog:null,command:{action:'list'}}})
        .then(result=>{if(active&&mine===order.current&&!pending.current)adopt(result,token);})
        .catch(value=>{if(active&&token===epoch.current)setError(errorText(value));})
        .finally(()=>{reading=false;});
    };
    refresh();const timer=setInterval(refresh,250);
    return()=>{active=false;clearInterval(timer);epoch.current++;};
  },[project]);
  /** Serialize user commands so a late poll cannot overwrite their reply. */
  async function perform(id:string,work:()=>Promise<RequirementsReply|null>){
    if(pending.current)return false;
    pending.current=true;setBusy(id);setError('');const token=epoch.current;order.current++;
    try{const next=await work();if(next)adopt(next,token);return true;}
    catch(value){if(token===epoch.current)setError(errorText(value));return false;}
    finally{if(token===epoch.current){pending.current=false;setBusy(null);lastRead.current=0;}}
  }
  /** Approve exactly the print the user reviewed: the hashes of the draft's compile. */
  const approve=(info:RequirementInfo,workspace=project)=>perform(info.id,async()=>{
    const hashes=reviewHashes(info);
    if(!hashes)throw new Error('The reviewed version is not available yet.');
    return call('approve_requirement',{workspace_id:workspace,expected_catalog:current.current?.catalog??null,id:info.id,hashes});
  });
  const remove=(info:RequirementInfo)=>perform(info.id,()=>call('delete_requirement',{expected_catalog:current.current?.catalog??null,id:info.id}));
  const startRun=(id:string,workspace=project)=>perform(id,async()=>{
    const next=await call('requirements',{workspace_id:workspace,expected_catalog:current.current?.catalog??null,command:{action:'start_run',id}});
    // A started run is pending even before the next list says so.
    running.current=true;return next;
  });
  const cancelRun=(runId:string)=>perform(runId,()=>call('requirements',{expected_catalog:null,command:{action:'cancel_run',run_id:runId}}));
  const crossCheck=(info:RequirementInfo,workspace=project??'')=>requestAdvisory(workspace,info);
  return {requirements:reply?.requirements??[],run:reply?.run??null,loaded:!!reply,error,busy,approve,remove,startRun,cancelRun,crossCheck,clearError:()=>setError('')};
}
