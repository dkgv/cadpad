import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon} from './ui';
import {indexedModels,loadSourceIndex,modelCaption,useSourceCatalogEpoch} from './source-catalog';
import {RequirementComposer,type RequirementPart} from './requirement-composer';
import {mentionedModels} from './requirement-tokens';
import './requirements.css';
import {useRequirementCatalog} from './requirement-catalog';
import {RequirementCard,QuestionFlow,type CardActions} from './requirement-cards';
import {RequirementReplayPanel,type RequirementReplay} from './requirement-replay';
import {groupByModel,pairText,questionsOf,statusRank,statusOf,summaryOf,type RequirementInfo} from './requirement-review';

export type RequirementModel={model:ModelRef;label:string;detail?:string};
export type ModelRef={entry:string;builder:string};
type ProjectPartsReply={model?:{parts?:{name?:string;source?:{file?:string};builder_function?:string}[]}};
const sameModel=(a?:ModelRef|null,b?:ModelRef|null)=>{const key=(model?:ModelRef|null)=>model?JSON.stringify([model.entry,model.builder]):'';return key(a)===key(b);};
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
export function Requirements({project,projectRunnable,models,focusedModel,modelViews,onCreate,onEdit,onChanged,onAsk,agentBusy=false}:{
  /** Workspace that owns the catalog (project root, or a standalone design). */
  project:string;
  /** The project workspace is itself a design that unassigned requirements can run on. */
  projectRunnable:boolean;
  models:RequirementModel[];
  focusedModel?:ModelRef|null;
  modelViews:()=>{entry:string;builder:string;workspace:string}[];
  /** `model` is the single target for now (first mention, else the focused model); `mentioned` lists every model the text mentions. */
  onCreate:(intent:string,model:ModelRef|null,mentioned:ModelRef[])=>Promise<void>|void;
  onEdit?:(id:string,intent:string,model:ModelRef|null,mentioned:ModelRef[])=>Promise<void>|void;
  onChanged:()=>void;
  /** Send a message to the assistant (answers, allowance drafts, rule removal). `mentioned` models are prepared first. */
  onAsk?:(request:string,mentioned:ModelRef[])=>Promise<void>|void;
  agentBusy?:boolean;
}){
  const catalog=useRequirementCatalog(project);
  const sourceCatalogEpoch=useSourceCatalogEpoch();
  const [partSuggestions,setPartSuggestions]=useState<RequirementPart[]>([]);
  const [error,setError]=useState('');
  const [requirementReplay,setRequirementReplay]=useState<RequirementReplay|null>(null);
  const [creating,setCreating]=useState(false),[intent,setIntent]=useState(''),[target,setTarget]=useState(''),[drafting,setDrafting]=useState(false),[editing,setEditing]=useState<string|null>(null);
  const [preparing,setPreparing]=useState('');
  const replayRequest=useRef(0);
  useEffect(()=>{setRequirementReplay(null);return()=>{replayRequest.current++;};},[project]);
  useEffect(()=>{
    let active=true;
    invoke<ProjectPartsReply>('project',{workspace:project}).then(reply=>{
      if(!active)return;
      const parts:RequirementPart[]=[];
      for(const part of reply.model?.parts??[]){
        if(!part.name)continue;
        const entry=part.source?.file;
        const builder=part.builder_function;
        const owners=entry?models.filter(item=>item.model.entry===entry):[];
        const owner=(builder?owners.find(item=>item.model.builder===builder):undefined)
          ??(owners.length===1?owners[0]:undefined)
          ??(models.length===1?models[0]:undefined)
          ??(focusedModel?models.find(item=>sameModel(item.model,focusedModel)):undefined);
        if(owner)parts.push({model:owner.model,modelLabel:owner.label,name:part.name,detail:owner.detail});
      }
      const unique=[...new Map(parts.map(item=>[`${JSON.stringify([item.model.entry,item.model.builder])}:${item.name}`,item])).values()];
      setPartSuggestions(current=>JSON.stringify(current)===JSON.stringify(unique)?current:unique);
    }).catch(()=>{if(active)setPartSuggestions([]);});
    return()=>{active=false;};
  },[project,models,focusedModel,sourceCatalogEpoch]);
  const latest=useRef({modelViews,onChanged,project});latest.current={modelViews,onChanged,project};
  const reqs=catalog.requirements;
  const count=reqs.length;
  const choices:{key:string;label:string;model:ModelRef|null}[]=[
    ...models.map(item=>({key:JSON.stringify([item.model.entry,item.model.builder]),label:item.label,model:item.model as ModelRef|null})),
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
    crossCheck:info=>catalog.crossCheck(info,requirementWorkspace(info)),
    ask:async(prompt,info)=>{if(!onAsk)throw new Error('The assistant is not available here.');await onAsk(prompt,mentionedRefs(info.text));},
    editWording:info=>composeRequirement(info),
    replay:async(info,primitiveId,worst)=>{
      const token=++replayRequest.current,sourceProject=latest.current.project;
      const replay=await invoke<RequirementReplay>('requirement_replay',{
        workspace:requirementWorkspace(info),
        request:{requirement_id:info.id,primitive_id:primitiveId,state:worst.state},
      });
      if(token!==replayRequest.current||latest.current.project!==sourceProject){
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
  function compose(){
    const fallback=choices[0];
    setEditing(null);setIntent('');setTarget(fallback?.key??'');setCreating(true);
  }
  const canSubmit=!!intent.trim()&&!agentBusy&&intent.length<=4096&&!!pickTarget(intent);
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
  /** Today's single-model flow: the first mentioned model that is still in the project, else the focused one. */
  function pickTarget(text:string){
    const mentioned=mentionedModels(text).filter(ref=>models.some(item=>sameModel(item.model,ref))) as ModelRef[];
    const choice=(mentioned.length?choices.find(item=>sameModel(item.model,mentioned[0])):undefined)??choices.find(item=>item.key===target)??choices[0];
    return choice?{choice,mentioned}:null;
  }
  // The assistant finished preparing: a requirement was saved, or it is waiting for answers.
  const draftCount=useRef(0);
  useEffect(()=>{if(drafting&&count!==draftCount.current)setDrafting(false);},[count]);
  useEffect(()=>{setCreating(false);setDrafting(false);setIntent('');setEditing(null);setPreparing('');setRequirementReplay(null);},[project]);
  const asking=reqs.find(item=>item.approval!=='approved'&&questionsOf(item).length>0);
  // Group by model in the order the project lists them; unknown models and unassigned last.
  const groups=new Map<string,{label:string;detail?:string;model:ModelRef|null;reqs:RequirementInfo[]}>();
  for(const item of models)groups.set(JSON.stringify([item.model.entry,item.model.builder]),{label:item.label,detail:item.detail,model:item.model,reqs:[]});
  // A requirement appears under every model its text mentions (P1.81).
  for(const [key,list] of groupByModel(reqs)){
    if(!groups.has(key)){
      const [entry,builder]=key?JSON.parse(key) as [string,string]:['',''];
      const model=key?{entry,builder}:null;
      groups.set(key,{label:model?labelOf(model):(projectRunnable?'This design':'Unassigned'),detail:model?.entry,model,reqs:[]});
    }
    groups.get(key)!.reqs.push(...list);
  }
  for(const group of groups.values())group.reqs.sort((a,b)=>statusRank(statusOf(a))-statusRank(statusOf(b)));
  const unassigned=groups.get('');if(unassigned){groups.delete('');groups.set('',unassigned);}
  function labelOf(model?:ModelRef|null){
    if(!model)return projectRunnable?'This design':'Unassigned';
    return models.find(item=>sameModel(item.model,model))?.label??model.entry.split('/').at(-1)??model.entry;
  }

  return <><section className="motion-tests">
    {!catalog.loaded&&!catalog.error&&<p className="muted">Loading requirements…</p>}
    {catalog.loaded&&count>0&&!creating&&<div className="test-heading test-list-heading"><span className="muted">{count} {count===1?'requirement':'requirements'}</span><button disabled={agentBusy||!choices.length} onClick={compose}><Icon name="plus" size={13}/>New requirement…</button></div>}
    {catalog.loaded&&!count&&!creating&&(drafting
      ?<div className="test-empty"><Icon name="sparkles" size={24}/><h3>{preparing||(agentBusy?'Preparing checks…':'Answer the assistant')}</h3><p>{preparing||agentBusy?'The assistant will save measurable checks here for you to review.':'The assistant needs more information before it can save checks. Reply in the Assistant panel.'}</p></div>
      :<div className="test-empty"><Icon name="tests" size={26}/><h3>No requirements yet</h3><p>{choices.length?'Describe a requirement for any model in this project. Review the checks, then check the requirement.':'Add a model to this project to define requirements for it.'}</p>{!!choices.length&&<button className="primary-button" disabled={agentBusy} onClick={compose}>New requirement…</button>}</div>)}
    {creating&&<form className="test-composer" onSubmit={event=>void submit(event)}>
      <RequirementComposer id="test-intent" label="What should the model do?" value={intent} onChange={setIntent} models={models} parts={partSuggestions} openModels={modelViews().map(view=>({entry:view.entry,builder:view.builder}))} autoFocus onSubmit={()=>{if(canSubmit)void submit({preventDefault(){}} as React.FormEvent);}} placeholder="@ a model or part: it should move at least 50 mm."/>
      <p className="muted">The assistant prepares steps and pass/fail checks for the mentioned models, even if they aren't open. You review them before running.</p>
      <div className="test-composer-actions"><button className="primary-button" type="submit" disabled={!canSubmit}>{editing?'Update checks':'Prepare requirement'}</button><button type="button" onClick={()=>setCreating(false)}>Cancel</button></div>
    </form>}
    {count>0&&drafting&&!creating&&<p className="muted" role="status">{preparing||(agentBusy?'The assistant is preparing checks…':'Waiting for your reply in the Assistant panel.')}</p>}
    {preparing&&!drafting&&<p className="muted" role="status">{preparing}</p>}
    {asking&&<QuestionFlow info={asking} questions={questionsOf(asking)} agentBusy={agentBusy} onSend={prompt=>cardActions.ask(prompt,asking)}/>}
    {reqs.length>0&&<div className="rq-summary" role="status" aria-label="Requirement summary">{summaryOf(reqs).map(({key,label,count:n})=><span key={key} className="rq-badge">{n} {label}</span>)}</div>}
    {[...groups.entries()].filter(([,group])=>group.reqs.length).map(([key,group])=><section className="requirement-group" key={key||'unassigned'} aria-label={`Requirements for ${group.label}`}>
      <header className="requirement-group-heading"><Icon name={group.model?'cube':'tests'} size={13}/><strong>{group.label}</strong>{group.detail&&<small title={group.detail}>{group.detail.split('/').at(-1)}</small>}{!!group.model&&sameModel(group.model,focusedModel)&&<span className="muted">· open</span>}</header>
      {group.reqs.map(item=><RequirementCard key={`${key}:${item.id}`} info={item} actions={cardActions}/>)}
    </section>)}
    {error&&<p role="alert" className="error">{error}</p>}
    {catalog.error&&catalog.error!==error&&<p role="alert" className="error">{catalog.error}</p>}
  </section>{requirementReplay&&<RequirementReplayPanel replay={requirementReplay} onClose={()=>setRequirementReplay(null)}/>}</>;
}
