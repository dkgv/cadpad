import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import type {MotionSnapshot} from '@cadpad/viewer';
import {Icon} from './ui';
import {indexedModels,loadSourceIndex,modelCaption,useSourceCatalogEpoch} from './source-catalog';
import {RequirementComposer} from './requirement-composer';
import {mentionedModels,tokenFor} from './requirement-tokens';
import './requirements.css';
import {useRequirementCatalog} from './requirement-catalog';
import {RequirementCard,QuestionFlow,type CardActions} from './requirement-cards';
import {RequirementReplayPanel,type RequirementReplay} from './requirement-replay';
import {groupByModel,pairText,questionsOf,statusRank,statusOf,summaryOf,type RequirementInfo} from './requirement-review';
import {MotionTestDefinitionView,type ModelRef,type MotionTestDefinition,type MotionTestOutcome,type MotionTestReview} from './motion-test-definition';

type Test={id:string;definition:MotionTestDefinition;outcome:MotionTestOutcome|null;review?:MotionTestReview|null;stale:boolean;design_compared?:boolean;frame_count:number;duration_s:number};
type Run={id:string;workspace_id:string;revision:string;test_id:string;actor:string;state:'queued'|'running'|'cancelling'|'completed'|'cancelled'|'failed';error:{code:string;message:string}|null};
type Reply={catalog:string;project_workspace?:string;tests:Test[];accuracy:string;snapshot:MotionSnapshot|null;run?:Run|null};
type View={workspace:string;revision:string};
type ReplayTarget={id:string;workspace:string;revision:string};
export type RequirementModel={model:ModelRef;label:string;detail?:string};
const activeRun=(run?:Run|null)=>!!run&&['queued','running','cancelling'].includes(run.state);
const modelKey=(model?:ModelRef|null)=>model?JSON.stringify([model.entry,model.builder]):'';
const sameModel=(a?:ModelRef|null,b?:ModelRef|null)=>modelKey(a)===modelKey(b);
const errorText=(value:unknown)=>value instanceof Error?value.message:typeof value==='object'&&value&&'message' in value?String((value as {message:unknown}).message):String(value);

/** Models listed in the project's source catalog (the Explorer's Models section). */
export function useProjectModels(workspace?:string,revision?:string):RequirementModel[]{
  const epoch=useSourceCatalogEpoch();
  const [models,setModels]=useState<RequirementModel[]>([]);
  useEffect(()=>{
    if(!workspace||!revision){setModels([]);return;}
    let active=true;
    loadSourceIndex(undefined,workspace,revision).then(index=>{
      if(!active)return;
      const next=indexedModels(index.files).map(({file,builder})=>({model:{entry:file.path,builder:builder.id},label:modelCaption(builder),detail:file.path}));
      setModels(current=>JSON.stringify(current)===JSON.stringify(next)?current:next);
    }).catch(()=>{if(active)setModels([]);});
    return()=>{active=false;};
  },[workspace,revision,epoch]);
  return models;
}

/**
 * Project requirements. One catalog per project; each requirement checks one model.
 * Lives in the shared sidebar, independent of the focused tab: models without a tab are
 * prepared in the background to run, and replay opens a separate temporary view.
 */
export function Requirements({project,projectRevision,projectRunnable,models,focusedModel,modelViews,ensureView,openModel,onSnapshot,onCreate,onEdit,onChanged,onAsk,agentBusy=false}:{
  /** Workspace that owns the catalog (project root, or a standalone design). */
  project:string;
  /** Revision of `project`; used to run requirements that check the project design itself. */
  projectRevision:string;
  /** The project workspace is itself a design that unassigned requirements can run on. */
  projectRunnable:boolean;
  models:RequirementModel[];
  focusedModel?:ModelRef|null;
  modelViews:()=>{entry:string;builder:string;workspace:string}[];
  ensureView?:(model:ModelRef)=>Promise<View>;
  openModel?:(model:ModelRef)=>Promise<void>|void;
  /** Replay snapshots, for a host that shows the replayed workspace itself. */
  onSnapshot?:(snapshot:MotionSnapshot)=>void;
  /** `model` is the single target for now (first mention, else the focused model); `mentioned` lists every model the text mentions. */
  onCreate:(intent:string,model:ModelRef|null,mentioned:ModelRef[])=>Promise<void>|void;
  onEdit?:(id:string,intent:string,model:ModelRef|null,mentioned:ModelRef[])=>Promise<void>|void;
  onChanged:()=>void;
  /** Send a message to the assistant (answers, allowance drafts, rule removal). `mentioned` models are prepared first. */
  onAsk?:(request:string,mentioned:ModelRef[])=>Promise<void>|void;
  agentBusy?:boolean;
}){
  const [reply,setReply]=useState<Reply|null>(null),[busy,setBusy]=useState(false),[error,setError]=useState('');
  const [requirementReplay,setRequirementReplay]=useState<RequirementReplay|null>(null);
  const [creating,setCreating]=useState(false),[intent,setIntent]=useState(''),[target,setTarget]=useState(''),[drafting,setDrafting]=useState(false),[editing,setEditing]=useState<string|null>(null);
  const [preparing,setPreparing]=useState(''),[starting,setStarting]=useState<string|null>(null),[stopRequested,setStopRequested]=useState(false);
  const [replayTarget,setReplayTarget]=useState<ReplayTarget|null>(null),[replayMotion,setReplayMotion]=useState<MotionSnapshot|null>(null),[frame,setFrame]=useState(0),[playing,setPlaying]=useState(false);
  const catalog=useRequirementCatalog(project);
  const latest=useRef({modelViews,onChanged,onSnapshot,project,projectRevision});latest.current={modelViews,onChanged,onSnapshot,project,projectRevision};
  const replayRequest=useRef(0);
  useEffect(()=>{setRequirementReplay(null);return()=>{replayRequest.current++;};},[project,projectRevision]);
  /** A start_run awaiting acceptance; Stop is queued until its run ID is known. */
  const pendingStart=useRef<{project:string;cancel:boolean}|null>(null);
  const epoch=useRef(0),sequence=useRef(0),pending=useRef(false),lastRead=useRef(0),runRef=useRef<Run|null>(null),notifiedRun=useRef(''),clock=useRef(0),draftCount=useRef(0);
  const viewIds=()=>latest.current.modelViews().map(view=>view.workspace);
  function adopt(result:Reply){
    if(!Array.isArray(result.tests))throw new Error('Requirements returned an invalid reply');
    setReply(result);runRef.current=result.run??null;
  }
  async function request(address:{workspace_id:string;revision:string},command:Record<string,unknown>,catalog:string|null){
    return invoke<Reply>('motion_tests',{request:{...address,expected_catalog:catalog,command,model_views:viewIds()}});
  }
  /** Serialize user commands; a reply from an older project or a superseded command is dropped. */
  async function perform(work:(token:number)=>Promise<Reply|null>){
    if(pending.current)return;pending.current=true;setBusy(true);setError('');
    const token=epoch.current,order=++sequence.current;
    try{
      const result=await work(token);
      if(result&&token===epoch.current&&order===sequence.current)adopt(result);
    }catch(value){if(token===epoch.current)setError(errorText(value));}
    finally{if(token===epoch.current){pending.current=false;setBusy(false);setPreparing('');setStarting(null);}}
  }
  const catalogAddress=()=>({workspace_id:project,revision:''});
  /** Where a requirement runs: its model's workspace, or the project design itself. */
  async function runAddress(test:Test):Promise<View>{
    const model=test.definition.model;
    if(!model){
      if(!projectRunnable)throw new Error('Assign this requirement to a model before checking it.');
      return {workspace:project,revision:projectRevision};
    }
    if(!ensureView)throw new Error('This model cannot be prepared here.');
    setPreparing(`Preparing ${labelOf(model)}…`);
    try{return await ensureView(model);}finally{setPreparing('');}
  }
  function labelOf(model?:ModelRef|null){
    if(!model)return projectRunnable?'This design':'Unassigned';
    return models.find(item=>sameModel(item.model,model))?.label??model.entry.split('/').at(-1)??model.entry;
  }
  async function start(test:Test){
    setPlaying(false);setStarting(test.id);setStopRequested(false);
    const catalog=reply?.catalog??null,owner=project;
    await perform(async token=>{
      const view=await runAddress(test);
      // The catalog CAS value is the project's, whichever model workspace runs the check.
      const queued={project:owner,cancel:false};pendingStart.current=queued;
      try{
        const result=await request({workspace_id:view.workspace,revision:view.revision},{action:'start_run',id:test.id},catalog);
        if(queued.cancel&&result.run){
          const stopped=await invoke<Reply>('motion_tests',{request:{workspace_id:queued.project,revision:'',expected_catalog:null,command:{action:'cancel_run',run_id:result.run.id},model_views:token===epoch.current?viewIds():[]}});
          return stopped;
        }
        if(token===epoch.current)runRef.current=result.run??null;
        return result;
      }finally{if(pendingStart.current===queued)pendingStart.current=null;}
    });
  }
  async function stop(){
    setStopRequested(true);setPlaying(false);
    const queued=pendingStart.current;
    if(queued&&queued.project===project){queued.cancel=true;return;}
    const run=runRef.current;if(!run){setStopRequested(false);return;}
    try{adopt(await request(catalogAddress(),{action:'cancel_run',run_id:run.id},null));}
    catch(value){setError(errorText(value));setStopRequested(false);}
  }
  async function remove(test:Test){
    await perform(async()=>{const result=await request(catalogAddress(),{action:'delete',id:test.id},reply?.catalog??null);latest.current.onChanged();return result;});
  }
  async function assign(test:Test,model:ModelRef){
    await perform(async()=>{const result=await request(catalogAddress(),{action:'upsert',id:test.id,definition:{...test.definition,model}},reply?.catalog??null);latest.current.onChanged();return result;});
  }
  async function replay(test:Test,index:number,known?:ReplayTarget){
    const model=test.definition.model;
    await perform(async token=>{
      let goal=known;
      if(!goal){
        if(model)await openModel?.(model);
        const view=await runAddress(test);
        goal={id:test.id,workspace:view.workspace,revision:view.revision};
      }
      const motion=await invoke<MotionSnapshot>('motion',{workspace:goal.workspace,revision:goal.revision});
      const result=await request({workspace_id:goal.workspace,revision:goal.revision},{action:'replay',id:test.id,frame_index:index,expected_pose_version:motion.pose_version},reply?.catalog??null);
      if(token===epoch.current&&result.snapshot){setReplayTarget(goal);setReplayMotion(result.snapshot);setFrame(result.snapshot.playback?.frame_index??index);latest.current.onSnapshot?.(result.snapshot);}
      return result;
    });
  }
  // Reset on project change, then poll: quickly during a run, otherwise every 2 s.
  useEffect(()=>{
    const token=++epoch.current;pending.current=false;
    setReply(null);setError('');setBusy(false);setCreating(false);setEditing(null);setDrafting(false);setIntent('');setPreparing('');setStarting(null);setStopRequested(false);setReplayTarget(null);setReplayMotion(null);setPlaying(false);
    runRef.current=null;pendingStart.current=null;notifiedRun.current='';lastRead.current=0;
    let active=true,reading=false;
    const refresh=()=>{
      if(reading||pending.current||(!activeRun(runRef.current)&&lastRead.current&&performance.now()-lastRead.current<2000))return;
      reading=true;lastRead.current=performance.now();const order=++sequence.current;
      invoke<Reply>('motion_tests',{request:{workspace_id:project,revision:'',expected_catalog:null,command:{action:'list'},model_views:viewIds()}})
        .then(result=>{if(active&&token===epoch.current&&order===sequence.current&&!pending.current)adopt(result);})
        .catch(value=>{if(active&&order===sequence.current)setError(errorText(value));})
        .finally(()=>{reading=false;});
    };
    refresh();const timer=setInterval(refresh,250);
    return()=>{active=false;clearInterval(timer);epoch.current++;};
  },[project]);
  useEffect(()=>{
    const run=reply?.run;
    if(run&&!activeRun(run)){
      setStopRequested(false);
      if(run.state==='completed'&&notifiedRun.current!==run.id){notifiedRun.current=run.id;latest.current.onChanged();}
    }
  },[reply?.run?.id,reply?.run?.state]);
  // The assistant finished preparing: a requirement was saved, or it is waiting for answers.
  const savedCount=(reply?.tests.length??0)+catalog.requirements.length;
  useEffect(()=>{if(drafting&&savedCount!==draftCount.current)setDrafting(false);},[savedCount]);
  // Follow the replayed model's playback so leaving playback in the canvas hides the timeline.
  useEffect(()=>{
    if(!replayTarget)return;
    let active=true;
    const timer=setInterval(()=>{
      void invoke<MotionSnapshot>('motion',{workspace:replayTarget.workspace,revision:replayTarget.revision}).then(motion=>{
        if(!active)return;
        if(motion.playback?.recording_id!==`motion-test:${replayTarget.id}`){setReplayTarget(null);setReplayMotion(null);setPlaying(false);}
        else{setReplayMotion(motion);if(!playing)setFrame(motion.playback.frame_index);}
      }).catch(()=>{if(active){setReplayTarget(null);setReplayMotion(null);setPlaying(false);}});
    },500);
    return()=>{active=false;clearInterval(timer);};
  },[replayTarget?.id,replayTarget?.workspace,replayTarget?.revision,playing]);
  const replayed=reply?.tests.find(test=>test.id===replayTarget?.id);
  useEffect(()=>{
    if(!playing||!replayed||!replayTarget)return;
    const timer=setInterval(()=>{
      if(pending.current)return;
      const index=Math.min(replayed.frame_count-1,Math.floor((performance.now()-clock.current)/1000*120));
      void replay(replayed,index,replayTarget);
      if(index===replayed.frame_count-1)setPlaying(false);
    },100);
    return()=>clearInterval(timer);
  },[playing,replayed?.id,replayed?.frame_count,replayTarget?.workspace,reply?.catalog]);

  const run=reply?.run;
  const executing=(id:string)=>starting===id&&!preparing||(activeRun(run)&&run?.test_id===id);
  const tests=reply?.tests??[];
  const reqs=catalog.requirements;
  const count=tests.length+reqs.length;
  const choices:{key:string;label:string;model:ModelRef|null}[]=[
    ...models.map(item=>({key:modelKey(item.model),label:item.label,model:item.model as ModelRef|null})),
    ...(projectRunnable?[{key:'',label:'This design',model:null}]:[]),
  ];
  const mentionedRefs=(text:string)=>mentionedModels(text).filter(ref=>models.some(item=>sameModel(item.model,ref))) as ModelRef[];
  const requirementWorkspace=(info:RequirementInfo)=>{
    const mentioned=mentionedRefs(info.text);
    const candidates=mentioned.length?mentioned:[focusedModel];
    return latest.current.modelViews().find(view=>candidates.some(model=>sameModel(model,view)))?.workspace??project;
  };
  const cardActions:CardActions={
    busy:catalog.busy,agentBusy,run:catalog.run,
    approve:info=>void catalog.approve(info,requirementWorkspace(info)).then(ok=>{if(ok)latest.current.onChanged();}),
    remove:info=>void catalog.remove(info),
    start:info=>void catalog.startRun(info.id,requirementWorkspace(info)),
    cancel:runId=>void catalog.cancelRun(runId),
    ask:async(prompt,info)=>{if(!onAsk)throw new Error('The assistant is not available here.');await onAsk(prompt,mentionedRefs(info.text));},
    editWording:info=>composeRequirement(info),
    replay:async(info,primitiveId,worst)=>{
      const token=++replayRequest.current,sourceProject=latest.current.project,sourceRevision=latest.current.projectRevision;
      const replay=await invoke<RequirementReplay>('requirement_replay',{
        workspace:requirementWorkspace(info),
        request:{requirement_id:info.id,primitive_id:primitiveId,state:worst.state},
      });
      if(token!==replayRequest.current||latest.current.project!==sourceProject||latest.current.projectRevision!==sourceRevision){
        if(replay.view?.temporary)void invoke('close_requirement_replay',{workspace:replay.view.workspace}).catch(()=>{});
        return;
      }
      setRequirementReplay({...replay,stateLabel:worst.state_label,pairLabel:pairText(worst.pair)});
    },
  };
  /** Edit wording of a requirement: the composer, seeded with its text (chips included), goes to the assistant as an edit. */
  function composeRequirement(info:RequirementInfo){
    const model=mentionedRefs(info.text)[0]??focusedModel??null;
    const fallback=choices.find(choice=>sameModel(choice.model,model))??choices[0];
    setEditing(info.id);setIntent(info.text);setTarget(fallback?.key??'');setCreating(true);
  }
  const canSubmit=!!intent.trim()&&!agentBusy&&intent.length<=4096&&!!pickTarget(intent);
  function compose(edit?:Test){
    const model=edit?edit.definition.model:focusedModel;
    const fallback=choices.find(choice=>sameModel(choice.model,model))??choices[0];
    // Seed the focused (or edited) model as a chip so the target is visible and easy to change.
    const seed=models.find(item=>sameModel(item.model,model));
    const chip=seed?tokenFor({label:seed.label,entry:seed.model.entry,builder:seed.model.builder})+' ':'';
    setEditing(edit?.id??null);setIntent(chip+(edit?.definition.intent??''));setTarget(fallback?.key??'');setCreating(true);
  }
  /** Today's single-model flow: the first mentioned model that is still in the project, else the focused one. */
  function pickTarget(text:string){
    const mentioned=mentionedModels(text).filter(ref=>models.some(item=>sameModel(item.model,ref))) as ModelRef[];
    const choice=(mentioned.length?choices.find(item=>sameModel(item.model,mentioned[0])):undefined)??choices.find(item=>item.key===target)??choices[0];
    return choice?{choice,mentioned}:null;
  }
  async function submit(event:React.FormEvent){
    event.preventDefault();
    const text=intent.trim(),pick=pickTarget(text);
    if(!text||agentBusy||!pick)return;
    const {choice,mentioned}=pick;
    draftCount.current=count;setCreating(false);setDrafting(true);setError('');
    try{
      if(choice.model)setPreparing(`Preparing ${choice.label}…`);
      if(editing&&onEdit)await onEdit(editing,text,choice.model,mentioned);else await onCreate(text,choice.model,mentioned);
    }catch(value){setDrafting(false);setError(errorText(value));}
    finally{setPreparing('');}
  }
  // Group by model in the order the project lists them; unknown models and unassigned last.
  const groups=new Map<string,{label:string;detail?:string;model:ModelRef|null;tests:Test[];reqs:RequirementInfo[]}>();
  for(const item of models)groups.set(modelKey(item.model),{label:item.label,detail:item.detail,model:item.model,tests:[],reqs:[]});
  for(const test of tests){
    const key=modelKey(test.definition.model);
    if(!groups.has(key))groups.set(key,{label:labelOf(test.definition.model),detail:test.definition.model?.entry,model:test.definition.model??null,tests:[],reqs:[]});
    groups.get(key)!.tests.push(test);
  }
  // A requirement appears under every model its text mentions (P1.81).
  for(const [key,list] of groupByModel(reqs)){
    if(!groups.has(key)){
      const [entry,builder]=key?JSON.parse(key) as [string,string]:['',''];
      const model=key?{entry,builder}:null;
      groups.set(key,{label:labelOf(model),detail:model?.entry,model,tests:[],reqs:[]});
    }
    groups.get(key)!.reqs.push(...list);
  }
  for(const group of groups.values())group.reqs.sort((a,b)=>statusRank(statusOf(a))-statusRank(statusOf(b)));
  const asking=reqs.find(item=>item.approval!=='approved'&&questionsOf(item).length>0);
  const unassigned=groups.get('');if(unassigned){groups.delete('');groups.set('',unassigned);}
  const status=(item:Test)=>executing(item.id)
    ?{kind:'running',text:stopRequested||run?.state==='cancelling'?'Stopping':starting===item.id?'Starting':run?.state==='queued'?'Queued':'Running'}
    :!item.outcome?{kind:'ready',text:'Ready to check'}
    :item.stale?{kind:'stale',text:'Needs recheck'}
    :{kind:item.outcome.passed?'passed':'failed',text:item.outcome.passed?'Met':'Not met'};
  const canRun=(item:Test)=>!!item.definition.model||projectRunnable;

  return <><section className="motion-tests">
    {!reply&&!error&&<p className="muted">Loading requirements…</p>}
    {reply&&count>0&&!creating&&<div className="test-heading test-list-heading"><span className="muted">{count} {count===1?'requirement':'requirements'}</span><button disabled={agentBusy||!choices.length} onClick={()=>compose()}><Icon name="plus" size={13}/>New requirement…</button></div>}
    {reply&&(catalog.loaded||!!catalog.error)&&!count&&!creating&&(drafting
      ?<div className="test-empty"><Icon name="sparkles" size={24}/><h3>{preparing||(agentBusy?'Preparing checks…':'Answer the assistant')}</h3><p>{preparing||agentBusy?'The assistant will save measurable checks here for you to review.':'The assistant needs more information before it can save checks. Reply in the Assistant panel.'}</p></div>
      :<div className="test-empty"><Icon name="tests" size={26}/><h3>No requirements yet</h3><p>{choices.length?'Describe a requirement for any model in this project. Review the checks, then check the requirement.':'Add a model to this project to define requirements for it.'}</p>{!!choices.length&&<button className="primary-button" disabled={agentBusy} onClick={()=>compose()}>New requirement…</button>}</div>)}
    {creating&&<form className="test-composer" onSubmit={event=>void submit(event)}>
      <RequirementComposer id="test-intent" label="What should the model do?" value={intent} onChange={setIntent} models={models} autoFocus onSubmit={()=>{if(canSubmit)void submit({preventDefault(){}} as React.FormEvent);}} placeholder="@ a model: it should move at least 50 mm."/>
      <p className="muted">The assistant prepares steps and pass/fail checks for the mentioned models, even if they aren't open. You review them before running.</p>
      <div className="test-composer-actions"><button className="primary-button" type="submit" disabled={!canSubmit}>{editing?'Update checks':'Prepare requirement'}</button><button type="button" onClick={()=>setCreating(false)}>Cancel</button></div>
    </form>}
    {count>0&&drafting&&!creating&&<p className="muted" role="status">{preparing||(agentBusy?'The assistant is preparing checks…':'Waiting for your reply in the Assistant panel.')}</p>}
    {preparing&&!drafting&&<p className="muted" role="status">{preparing}</p>}
    {(starting&&!preparing||activeRun(run))&&<div className="test-run-status" role="status"><strong>{stopRequested||run?.state==='cancelling'?'Stopping check…':starting&&!activeRun(run)?'Starting check…':run?.state==='queued'?'Check queued':'Checking requirement…'}</strong><button disabled={stopRequested||run?.state==='cancelling'} onClick={()=>void stop()}>Stop check</button></div>}
    {run&&!activeRun(run)&&run.state!=='completed'&&<p className="muted" role="status">{run.state==='cancelled'?'Check stopped. Previous results preserved.':`Check could not finish: ${run.error?.message??'Run failed'}`}</p>}
    {asking&&<QuestionFlow info={asking} questions={questionsOf(asking)} agentBusy={agentBusy} onSend={prompt=>cardActions.ask(prompt,asking)}/>}
    {reqs.length>0&&<div className="rq-summary" role="status" aria-label="Requirement summary">{summaryOf(reqs).map(({key,label,count:n})=><span key={key} className="rq-badge">{n} {label}</span>)}</div>}
    {[...groups.entries()].filter(([,group])=>group.tests.length||group.reqs.length).map(([key,group])=><section className="requirement-group" key={key||'unassigned'} aria-label={`Requirements for ${group.label}`}>
      <header className="requirement-group-heading"><Icon name={group.model?'cube':'tests'} size={13}/><strong>{group.label}</strong>{group.detail&&<small title={group.detail}>{group.detail.split('/').at(-1)}</small>}{!!group.model&&sameModel(group.model,focusedModel)&&<span className="muted">· open</span>}</header>
      {group.reqs.map(item=><RequirementCard key={`${key}:${item.id}`} info={item} actions={cardActions}/>)}
      {group.tests.map(item=>{const state=status(item);return <article className="motion-test" key={item.id}>
        <div className="test-heading"><strong>{item.definition.name}</strong><span className={`test-status ${state.kind}`} title={item.outcome&&!item.stale&&item.design_compared===false?'Last result. The model is not loaded, so design changes since then are not compared yet.':undefined}>{state.text}</span></div>
        {!item.definition.model&&!projectRunnable&&<div className="requirement-assign"><label htmlFor={`assign-${item.id}`}>Assign to model</label><select id={`assign-${item.id}`} defaultValue="" disabled={busy||!models.length} onChange={event=>{const choice=models.find(model=>modelKey(model.model)===event.target.value);if(choice)void assign(item,choice.model);}}><option value="" disabled>Choose a model…</option>{models.map(model=><option key={modelKey(model.model)} value={modelKey(model.model)}>{model.label}</option>)}</select></div>}
        <details className="test-original-requirement"><summary>Original requirement</summary><p className="test-intent">{item.definition.intent}</p></details>
        <MotionTestDefinitionView definition={item.definition} review={item.review} outcome={item.outcome} previous={item.stale||executing(item.id)}/>
        <div className="test-actions"><button className="primary-button" disabled={busy||activeRun(run)||!canRun(item)} onClick={()=>void start(item)}>{item.outcome?'Check again':'Check requirement'}</button>
        {item.frame_count>0&&<button disabled={busy||item.stale||!canRun(item)} title={item.definition.model&&!sameModel(item.definition.model,focusedModel)?'Opens the model to show the replay':undefined} onClick={()=>{setPlaying(false);void replay(item,0);}}>Inspect replay</button>}
        <span className="test-actions-spacer"/>
        {onEdit&&<button className="icon-button" title="Edit requirement with assistant" aria-label={`Edit requirement ${item.definition.name}`} disabled={busy||agentBusy||activeRun(run)} onClick={()=>compose(item)}><Icon name="edit" size={14}/></button>}
        <button className="icon-button" title="Delete requirement" aria-label={`Delete requirement ${item.definition.name}`} disabled={busy||activeRun(run)} onClick={()=>void remove(item)}><Icon name="close" size={14}/></button></div>
      </article>;})}
    </section>)}
    {replayed&&replayTarget&&replayMotion?.playback?.recording_id===`motion-test:${replayed.id}`&&!replayed.stale&&<div className="test-timeline"><strong>{replayed.definition.name}</strong><label>Replay · {(frame/120).toFixed(3)} / {replayed.duration_s.toFixed(3)} s<input type="range" aria-label="Requirement replay time" min={0} max={replayed.frame_count-1} step={1} value={frame} disabled={busy} onChange={event=>{setPlaying(false);void replay(replayed,Number(event.target.value),replayTarget);}}/></label><button disabled={busy&&!playing} onClick={()=>{if(playing)setPlaying(false);else{clock.current=performance.now()-(frame>=replayed.frame_count-1?0:frame/120*1000);setPlaying(true);}}}>{playing?'Pause replay':'Play replay'}</button><p className="muted">Choose Leave playback in the model's Move panel to return to live movement.</p></div>}
    {reply&&count>0&&<details className="test-simulation-limits muted"><summary>Simulation limits</summary><p>Results apply to the saved simulation setup.</p><p>{reply.accuracy}</p></details>}
    {error&&<p role="alert" className="error">{error}</p>}
    {catalog.error&&catalog.error!==error&&<p role="alert" className="error">{catalog.error}</p>}
  </section>{requirementReplay&&<RequirementReplayPanel replay={requirementReplay} onClose={()=>setRequirementReplay(null)}/>}</>;
}
