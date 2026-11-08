import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import type {MotionSnapshot} from '@cadpad/viewer';
import {Icon} from './ui';
import {MotionTestDefinitionView,type MotionTestDefinition,type MotionTestOutcome,type MotionTestReview} from './motion-test-definition';

type Test={id:string;definition:MotionTestDefinition;outcome:MotionTestOutcome|null;review?:MotionTestReview|null;stale:boolean;frame_count:number;duration_s:number};
type Run={id:string;workspace_id:string;revision:string;test_id:string;actor:string;state:'queued'|'running'|'cancelling'|'completed'|'cancelled'|'failed';error:{code:string;message:string}|null};
type Reply={catalog:string;tests:Test[];accuracy:string;snapshot:MotionSnapshot|null;run?:Run|null};
type PendingRun={testId?:string;workspace:string;revision:string;epoch:number;cancelRequested:boolean;run?:Run|null;stopping?:Promise<Reply>};
const activeRun=(run?:Run|null)=>!!run&&['queued','running','cancelling'].includes(run.state);
export function Requirements({workspace,revision,motion,onSnapshot,onCreate,onChanged,onEdit,agentBusy=false}:{workspace:string;revision:string;motion:MotionSnapshot|null;onSnapshot:(value:MotionSnapshot)=>void;onCreate:(intent:string)=>void;onChanged:()=>void;onEdit?:(id:string,intent:string)=>void;agentBusy?:boolean}){
  const [reply,setReply]=useState<Reply|null>(null),[busy,setBusy]=useState(false),[error,setError]=useState('');
  const [selected,setSelected]=useState<string|null>(null),[frame,setFrame]=useState(0),[playing,setPlaying]=useState(false);
  const [creating,setCreating]=useState(false),[intent,setIntent]=useState(''),[drafting,setDrafting]=useState(false),[editing,setEditing]=useState<string|null>(null);
  const latest=useRef({workspace,revision,motion,onSnapshot,onChanged});latest.current={workspace,revision,motion,onSnapshot,onChanged};
  const pending=useRef<number|null>(null),epoch=useRef(0),clock=useRef(0),sequence=useRef(0),lastRead=useRef(0);
  const pendingRun=useRef<PendingRun|null>(null),runRef=useRef<Run|null>(null),notifiedRun=useRef('');
  const [starting,setStarting]=useState(false),[stopRequested,setStopRequested]=useState(false);
  function adopt(result:Reply){setReply(result);runRef.current=result.run??null;}
  async function stopAccepted(run:PendingRun){
    if(!run.run)return;
    const order=run.epoch===epoch.current?++sequence.current:0;
    const result=await(run.stopping??=invoke<Reply>('motion_tests',{request:{workspace_id:run.workspace,revision:run.revision,expected_catalog:null,command:{action:'cancel_run',run_id:run.run.id}}}));
    if(run.epoch===epoch.current&&order===sequence.current)adopt(result);
  }
  async function stop(){
    setStopRequested(true);setPlaying(false);
    const scope=latest.current;
    const queued=pendingRun.current;
    const target=queued?.workspace===scope.workspace?queued:runRef.current?{workspace:scope.workspace,revision:scope.revision,epoch:epoch.current,cancelRequested:true,run:runRef.current}:null;
    if(!target)return;
    target.cancelRequested=true;
    try{await stopAccepted(target);}catch(value){if(target.epoch===epoch.current){setError(value instanceof Error?value.message:JSON.stringify(value));setStopRequested(false);}}
  }
  async function command(command:Record<string,unknown>){
    if(pending.current===epoch.current)return;pending.current=epoch.current;setBusy(true);setError('');
    const scope=latest.current,token=epoch.current,order=++sequence.current;
    const start:PendingRun|null=command.action==='start_run'?{testId:typeof command.id==='string'?command.id:undefined,workspace:scope.workspace,revision:scope.revision,epoch:token,cancelRequested:false}:null;
    if(start){pendingRun.current=start;setStarting(true);setStopRequested(false);}
    try{
      const result=await invoke<Reply>('motion_tests',{request:{workspace_id:scope.workspace,revision:scope.revision,expected_catalog:reply?.catalog??null,command}});
      if(start){start.run=result.run;if(start.cancelRequested)await stopAccepted(start);}
      if(token!==epoch.current||order!==sequence.current)return;
      if(!Array.isArray(result.tests))throw new Error('Requirements returned an invalid reply');
      adopt(result);
      if(result.snapshot){scope.onSnapshot(result.snapshot);setFrame(result.snapshot.playback?.frame_index??0);}
      if(['run','delete','upsert'].includes(String(command.action)))scope.onChanged();
      if(command.action==='delete'&&command.id===selected){setSelected(null);setPlaying(false);}
    }catch(value){if(token===epoch.current){setError(value instanceof Error?value.message:JSON.stringify(value));setPlaying(false);}}
    finally{if(pending.current===token)pending.current=null;if(start&&pendingRun.current===start)pendingRun.current=null;if(token===epoch.current){setBusy(false);setStarting(false);}}
  }
  useEffect(()=>{
    const token=++epoch.current;pending.current=null;setReply(null);setSelected(null);setPlaying(false);setFrame(0);setError('');setBusy(false);setCreating(false);setEditing(null);setDrafting(false);setIntent('');setStarting(false);setStopRequested(false);runRef.current=null;notifiedRun.current='';lastRead.current=0;
    let active=true,reading=false;
    const refresh=()=>{
      if(reading||pending.current===token||(!activeRun(runRef.current)&&performance.now()-lastRead.current<2000&&lastRead.current))return;
      reading=true;lastRead.current=performance.now();const order=++sequence.current;
      return invoke<Reply>('motion_tests',{request:{workspace_id:workspace,revision,expected_catalog:null,command:{action:'list'}}}).then(result=>{if(!Array.isArray(result.tests))throw new Error('Requirements returned an invalid reply');if(active&&token===epoch.current&&order===sequence.current&&pending.current!==token)adopt(result);}).catch(value=>{if(active&&order===sequence.current)setError(String(value));}).finally(()=>{reading=false;});
    };
    void refresh();const timer=setInterval(()=>void refresh(),250);
    return()=>{active=false;clearInterval(timer);epoch.current++;};
  },[workspace,revision]);
  useEffect(()=>{
    const playback=motion?.playback;
    if(playback?.recording_id.startsWith('motion-test:')){setSelected(playback.recording_id.slice('motion-test:'.length));setFrame(playback.frame_index);}
    else setPlaying(false);
  },[motion?.playback?.recording_id,motion?.playback?.frame_index]);
  useEffect(()=>{
    const run=reply?.run;
    if(run&&!activeRun(run)){
      setStopRequested(false);
      if(run.state==='completed'&&notifiedRun.current!==run.id){notifiedRun.current=run.id;latest.current.onChanged();}
    }
  },[reply?.run?.id,reply?.run?.state]);
  useEffect(()=>{if(reply?.tests.length)setDrafting(false);},[reply?.tests.length]);
  const executing=(id:string)=>(starting&&pendingRun.current?.testId===id)||(activeRun(reply?.run)&&reply?.run?.test_id===id);
  const test=reply?.tests.find(test=>test.id===selected);
  async function replay(id:string,index:number){
    const snapshot=latest.current.motion;if(!snapshot)return;
    setSelected(id);
    await command({action:'replay',id,frame_index:index,expected_pose_version:snapshot.pose_version});
  }
  useEffect(()=>{
    if(!playing||!test)return;
    const timer=setInterval(()=>{
      if(pending.current===epoch.current)return;
      const index=Math.min(test.frame_count-1,Math.floor((performance.now()-clock.current)/1000*120));
      void replay(test.id,index);
      if(index===test.frame_count-1)setPlaying(false);
    },100);
    return()=>clearInterval(timer);
  },[playing,test?.id,test?.frame_count,reply?.catalog]);
  const count=reply?.tests.length??0;
  return <section className="motion-tests">
    {!reply&&!error&&<p className="muted">Loading requirements…</p>}
    {reply&&count>0&&<div className="test-heading test-list-heading"><span className="muted">{count} {count===1?'requirement':'requirements'}</span><button disabled={agentBusy||creating} onClick={()=>{setEditing(null);setIntent('');setCreating(true);}}><Icon name="plus" size={13}/>New requirement…</button></div>}
    {reply&&!count&&!creating&&(drafting?<div className="test-empty"><Icon name="sparkles" size={24}/><h3>{agentBusy?'Preparing checks…':'Finish setup in Assistant'}</h3><p>The assistant will save measurable checks here for you to review.</p></div>:<div className="test-empty"><Icon name="tests" size={26}/><h3>No requirements yet</h3><p>Describe a requirement. Review the checks, then check the requirement.</p><button className="primary-button" disabled={agentBusy} onClick={()=>{setEditing(null);setIntent('');setCreating(true);}}>New requirement…</button></div>)}
    {creating&&<form className="test-composer" onSubmit={event=>{event.preventDefault();if(intent.trim()&&!agentBusy){setDrafting(true);if(editing&&onEdit)onEdit(editing,intent.trim());else onCreate(intent.trim());setCreating(false);}}}><label htmlFor="test-intent">What should the model do?</label><textarea id="test-intent" autoFocus value={intent} maxLength={4096} onChange={event=>setIntent(event.target.value)} placeholder="The carriage should move at least 50 mm."/><p className="muted">The assistant prepares steps and pass/fail checks. You review them before running.</p><div className="test-composer-actions"><button className="primary-button" type="submit" disabled={!intent.trim()||agentBusy}>{editing?'Update checks':'Prepare requirement'}</button><button type="button" onClick={()=>setCreating(false)}>Cancel</button></div></form>}
    {(starting||activeRun(reply?.run))&&<div className="test-run-status" role="status"><strong>{stopRequested||reply?.run?.state==='cancelling'?'Stopping check…':starting?'Starting check…':reply?.run?.state==='queued'?'Check queued':'Checking requirement…'}</strong><button disabled={stopRequested||reply?.run?.state==='cancelling'} onClick={()=>void stop()}>Stop check</button></div>}
    {reply?.run&&!activeRun(reply.run)&&reply.run.state!=='completed'&&<p className="muted" role="status">{reply.run.state==='cancelled'?'Check stopped. Previous results preserved.':`Check could not finish: ${reply.run.error?.message??'Run failed'}`}</p>}
    {reply?.tests.map(item=><article className="motion-test" key={item.id}>
      <div className="test-heading"><strong>{item.definition.name}</strong><span className={`test-status ${executing(item.id)?'running':!item.outcome?'ready':item.stale?'stale':item.outcome.passed?'passed':'failed'}`}>{executing(item.id)?(stopRequested||reply?.run?.state==='cancelling'?'Stopping':starting?'Starting':reply?.run?.state==='queued'?'Queued':'Running'):!item.outcome?'Ready to check':item.stale?'Needs recheck':item.outcome.passed?'Met':'Not met'}</span></div>
      <details className="test-original-requirement"><summary>Original requirement</summary><p className="test-intent">{item.definition.intent}</p></details>
      <MotionTestDefinitionView definition={item.definition} review={item.review} outcome={item.outcome} previous={item.stale||executing(item.id)}/>
      <div className="test-actions"><button className="primary-button" disabled={busy||activeRun(reply?.run)} onClick={()=>{setPlaying(false);void command({action:'start_run',id:item.id});}}>{item.outcome?'Check again':'Check requirement'}</button>
      {item.frame_count>0&&<button disabled={busy||item.stale||!!motion?.recording} onClick={()=>{setPlaying(false);void replay(item.id,0);}}>Inspect replay</button>}
      <span className="test-actions-spacer"/>
      {onEdit&&<button className="icon-button" title="Edit requirement with assistant" aria-label={`Edit requirement ${item.definition.name}`} disabled={busy||agentBusy||activeRun(reply?.run)} onClick={()=>{setEditing(item.id);setIntent(item.definition.intent);setCreating(true);}}><Icon name="edit" size={14}/></button>}
      <button className="icon-button" title="Delete requirement" aria-label={`Delete requirement ${item.definition.name}`} disabled={busy||activeRun(reply?.run)} onClick={()=>void command({action:'delete',id:item.id})}><Icon name="close" size={14}/></button></div>
    </article>)}
    {test&&!test.stale&&motion?.playback?.recording_id===`motion-test:${test.id}`&&<div className="test-timeline"><strong>{test.definition.name}</strong><label>Replay · {(frame/120).toFixed(3)} / {test.duration_s.toFixed(3)} s<input type="range" aria-label="Requirement replay time" min={0} max={test.frame_count-1} step={1} value={frame} disabled={busy} onChange={event=>{setPlaying(false);void replay(test.id,Number(event.target.value));}}/></label><button disabled={busy&&!playing} onClick={()=>{if(playing)setPlaying(false);else{clock.current=performance.now()-(frame>=test.frame_count-1?0:frame/120*1000);setPlaying(true);}}}>{playing?'Pause replay':'Play replay'}</button><p className="muted">Choose Leave playback above to return to live movement.</p></div>}
    {reply&&count>0&&<details className="test-simulation-limits muted"><summary>Simulation limits</summary><p>Results apply to the saved simulation setup.</p><p>{reply.accuracy}</p></details>}
    {error&&<p role="alert" className="error">{error}</p>}
  </section>;
}
