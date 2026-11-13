import {connectNativeStream,messageChannel} from './native-stream';
import {JointWizard} from './joint-wizard';
import {createPortal} from 'react-dom';
import type {SessionHost,ToolTab} from './session-host';
import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { invoke, Channel } from '@tauri-apps/api/core';
import { CadViewport, sceneEntity, sameEntity, referenceLetter, recordedFrameAt, type Inspection, type Entity, type SceneModel, type SelectionMode, type Scope, type MotionSnapshot } from '@cadpad/viewer';
import './style.css';
import {Icon,humanLabel,errorSummary,PropertySummary,type IconName} from './ui';
import {ExternalLoads} from './loads';
import {PhysicsSettings} from './physics-settings';
import {Requirements} from './motion-tests';
import {JointControl} from './joint-control';
import {PanelResizeHandle,usePanelSizes} from './panel-resize';
import {AssistantChatDock,chatTabIds,type ChatSummary} from './assistant-chats';
import {MotionParts} from './motion-parts';
import {BuildDiagnostics} from './build-diagnostics';
import {ProjectEntry,type EntrySelection} from './project-entry';
import {ModelHistory} from './model-history';
import {ExternalChangesDialog} from './external-changes';
import {AppAlert} from './app-alert';
import {ProjectExplorer} from './explorer';
import {SourceCatalog} from './source-catalog';
import {QuickModelSelector} from './quick-model';
import {LiveReply,FinishedReply,Markdown,type AgentStep} from './agent-steps';
import {useDockDrag} from './dock-drag';
import {ViewCube,type ViewCubeDirection} from './view-cube';
import {SettingsDialog,type AgentSettings,type McpSettings,type ModelChoice} from './settings-dialog';

const ConversationScroll=React.forwardRef<HTMLDivElement,React.PropsWithChildren>(function ConversationScroll({children},forwardedRef){
  const nodeRef=useRef<HTMLDivElement|null>(null);
  const follow=useRef(true),interacting=useRef(false);
  useLayoutEffect(()=>{
    const node=nodeRef.current;if(!node)return;
    const bottom=()=>{if(follow.current&&node.clientHeight>0)node.scrollTop=node.scrollHeight;};
    const resize=new ResizeObserver(bottom);
    resize.observe(node);if(node.firstElementChild)resize.observe(node.firstElementChild);
    const changes=new MutationObserver(bottom);changes.observe(node,{childList:true,subtree:true,characterData:true});
    bottom();const frame=requestAnimationFrame(bottom);
    return()=>{resize.disconnect();changes.disconnect();cancelAnimationFrame(frame);};
  },[]);
  return <div className="conversation" ref={node=>{nodeRef.current=node;if(typeof forwardedRef==='function')forwardedRef(node);else if(forwardedRef)forwardedRef.current=node;}}
    onWheel={()=>{interacting.current=true;}} onTouchStart={()=>{interacting.current=true;}} onPointerDown={()=>{interacting.current=true;}}
    onScroll={event=>{if(interacting.current){const node=event.currentTarget;follow.current=node.scrollHeight-node.scrollTop-node.clientHeight<48;}}}>{children}</div>;
});

type Parameter = {value:number;unit:string;minimum:number|null;maximum:number|null;description:string};
type Recording = {id:string;name:string;revision:string;source_mode:string;frame_count:number;duration_s:number;capacity_reached:boolean};
type PlaybackReply={snapshot:MotionSnapshot;recordings:Recording[];frame_times_s:number[]|null};
export type Project = { unsaved_changes?:boolean;model_parent?:string;model_source_dirty?:boolean;model_entry?:{entry:string;builder:string}|null; import_candidates?:string[]; display_generation?:string;display_error?:{code:string;message:string}|null;external_tracking:boolean; recordings:Recording[]; workspace_id:string; revision:string; files?:string[]; assets?:Record<string,string>; entry_point?:string; model:SceneModel; parameters:Record<string,Parameter>;activity:Job[]; name:string|null; saved_revision:string|null; dirty:boolean;motion:MotionSnapshot };
/** Model views create private adapter revisions without editing the user's project. */
export function hasUnsavedChanges(project:Project){
  return project.unsaved_changes??(project.model_parent?!!project.model_source_dirty:project.dirty);
}
type Job = { id:string;state:string;diagnostics:unknown;actor?:string };
type AgentTurn = {id:string;workspace_id:string;chat_id?:string;state:'running'|'cancelling'|'cancelled'|'succeeded'|'failed';text:string;activity:string;error:{code:string;message:string}|null;artifacts:{handle:string;format:string;revision:string}[];steps?:AgentStep[];started_at?:number};
type PendingAgentStart={workspace:string;cancelRequested:boolean;turn?:AgentTurn;stopping?:Promise<AgentTurn>};
type ChatReference={letter:string;instance_id:string;entity_id:string;part:string;label:string;file?:string;line?:number};
type ChatMessage = {image?:string;checkpoint_revision?:string;role:string;text:string;references?:ChatReference[];steps?:AgentStep[]};
const MAX_REFERENCES=8;
const MAX_ATTACHED_IMAGE=1024*1024;
const entityKey=(entity:Entity)=>`${entity.revision}|${entity.instance_id}|${entity.entity_id}`;
type AttachedImage={data:string;preview:string;name:string};
/** Encode a dropped laptop image as a bounded base64 PNG for agent_start. */
async function encodeReferenceImage(file:File):Promise<AttachedImage>{
  if(!file.type.startsWith('image/'))throw new Error('Drop an image file to attach it to your next message.');
  const source='createImageBitmap' in globalThis?await createImageBitmap(file):await new Promise<HTMLImageElement>((resolve,reject)=>{
    const url=URL.createObjectURL(file),image=new Image();
    image.onload=()=>{URL.revokeObjectURL(url);resolve(image);};
    image.onerror=()=>{URL.revokeObjectURL(url);reject(new Error('Could not read that image.'));};
    image.src=url;
  });
  const canvas=document.createElement('canvas'),context=canvas.getContext('2d');
  if(!context)throw new Error('Could not prepare that image.');
  try{
    for(const scale of [1,.75,.5,.35,.25,.15]){
      canvas.width=Math.max(1,Math.round(source.width*scale));
      canvas.height=Math.max(1,Math.round(source.height*scale));
      context.clearRect(0,0,canvas.width,canvas.height);
      context.drawImage(source,0,0,canvas.width,canvas.height);
      const preview=canvas.toDataURL('image/png'),data=preview.slice(preview.indexOf(',')+1);
      if(data.length<=MAX_ATTACHED_IMAGE)return {data,preview,name:file.name||'reference.png'};
    }
  }finally{if('close' in source)(source as ImageBitmap).close();}
  throw new Error('That image is too large to attach (max 1 MiB).');
}
type ViewStatus={workspace:string;revision:string;generation?:string;phase:'loading'|'ready'|'missing'|'failed';message?:string};
type CameraAction=ViewCubeDirection|'fit';
type InterferenceResult={revision:string;pose_version:number;exact:{pairs:{instances:[string,string];distance_mm:number;intersection_volume_mm3:number;interfering:boolean}[]}|null};
const message = (error: unknown) => error instanceof Error ? error.message : typeof error === 'object' ? JSON.stringify(error,null,2) : String(error);
const errorCode=(error:unknown)=>{
  if(error&&typeof error==='object'&&'code' in error&&typeof (error as {code:unknown}).code==='string')return (error as {code:string}).code;
  try{const data=JSON.parse(message(error));return typeof data?.code==='string'?data.code:typeof data?.error?.code==='string'?data.error.code:'';}catch{return '';}
};
const motionTools=[
  {id:'move',title:'Simulation',button:'tab-motion',icon:'motion'},
  {id:'simulate',title:'Physics',button:'tab-physics',icon:'physics'},
  {id:'test',title:'Requirements',button:'tab-tests',icon:'tests'},
] as const;
type MotionTask=typeof motionTools[number]['id'];
export function App({host}:{host?:SessionHost}={}) {
  const hostRef=useRef(host);hostRef.current=host;
  const dockDrag=useDockDrag();
  const railEvents=(tool:ToolTab,label:string)=>({
    onPointerDown:(event:React.PointerEvent<HTMLButtonElement>)=>{
      event.currentTarget.dataset.tooltipDismissed='true';
      if(!host||!dockDrag)return;
      const home: 'left'|'right'=tool==='assistant'?'right':'left';
      dockDrag.begin(event,{kind:'tab',tabs:[tool],active:tool,area:home,group:home,label});
    },
    onPointerLeave:(event:React.PointerEvent<HTMLButtonElement>)=>{delete event.currentTarget.dataset.tooltipDismissed;},
    onKeyDown:(event:React.KeyboardEvent<HTMLButtonElement>)=>{if(event.key==='Enter'||event.key===' ')event.currentTarget.dataset.tooltipDismissed='true';},
    onBlur:(event:React.FocusEvent<HTMLButtonElement>)=>{delete event.currentTarget.dataset.tooltipDismissed;},
  });
  const [panelSizes,resizePanel]=usePanelSizes();
  const hoverStatus=useRef<HTMLSpanElement>(null);
  const container = useRef<HTMLDivElement>(null), viewer = useRef<CadViewport|null>(null);
  const sessionAlive=useRef(true);
  useEffect(()=>{sessionAlive.current=true;return()=>{sessionAlive.current=false;};},[]);
  const [project,setProject] = useState<Project|null>(null), [job,setJob] = useState<Job|null>(null);
  /** Chrome Explorer follows the focused canvas model without remounting shared sidebars. */
  const [focusProject,setFocusProject]=useState<Project|null>(null);
  const [error,setError] = useState(''), [selection,updateSelection] = useState<Entity|null>(null);
  const selectionIntent=useRef<{workspace:string;entity:Entity}|null>(null);
  // `references` is everything selected, in click order; `selection` is the
  // primary entity the inspector acts on (the latest click).
  const [references,updateReferences]=useState<Entity[]>([]),referencesNow=useRef<Entity[]>([]);
  const [omitted,setOmitted]=useState<Set<string>>(new Set());
  const [attachedImage,setAttachedImage]=useState<AttachedImage|null>(null);
  const [imageDropDepth,setImageDropDepth]=useState(0);
  function setReferences(next:Entity[]){referencesNow.current=next;updateReferences(next);}
  function choosePrimary(entity:Entity|null){const current=currentProject.current;selectionIntent.current=entity&&current?{workspace:current.workspace_id,entity}:null;updateSelection(entity);}
  /** Replaces the selection, keeping the reference set when `entity` already belongs to it. */
  function setSelection(entity:Entity|null,replace=false){
    choosePrimary(entity);
    if(entity&&!toolPanelOpen.current){if(hostRef.current)hostRef.current.showTool('model');else setModelOpen(true);}
    const now=referencesNow.current;
    setReferences(!entity?[]:!replace&&now.some(item=>sameEntity(item,entity))?now.filter(item=>item.revision===entity.revision):[entity]);
  }
  /** Shift/⌘-click: add, or remove when already selected. The latest addition becomes primary. */
  function toggleReference(entity:Entity){
    const now=referencesNow.current.filter(item=>item.revision===entity.revision);
    const present=now.some(item=>sameEntity(item,entity));
    if(!present&&now.length>=MAX_REFERENCES){setSelectionNotice(`Select up to ${MAX_REFERENCES} references per message.`);return;}
    const next=present?now.filter(item=>!sameEntity(item,entity)):[...now,entity];
    const primary=present?next[next.length-1]??null:entity;
    setReferences(next);choosePrimary(primary);viewer.current?.selectEntity(primary);
  }
  const [graphicsLost,setGraphicsLost]=useState(false);
  const [startupError,setStartupError]=useState(''),[viewportAttempt,setViewportAttempt]=useState(0);
  const [viewStatus,setViewStatus]=useState<ViewStatus|null>(null),[viewAttempt,setViewAttempt]=useState(0);
  const cameraIntent=useRef<{workspace:string;revision:string;action:CameraAction}|null>(null);
  const [properties,setProperties] = useState<Inspection|null>(null);
  const [inspectStatus,setInspectStatus]=useState('');
  const [selectionMode,setSelectionMode]=useState<SelectionMode>('face');
  const [panel,setFocusedTool]=useState<'model'|'motion'|'assistant'>('assistant');
  const [modelOpen,setModelOpen]=useState(true),[motionOpen,setMotionOpen]=useState(false),[assistantOpen,setAssistantOpen]=useState(true);
  function setPanel(target:'model'|'motion'|'assistant'){
    setFocusedTool(target);
    if(hostRef.current){hostRef.current.showTool(target==='motion'?motionTask:target);if(target==='assistant')requestAnimationFrame(()=>document.getElementById('model-prompt')?.focus());return;}
    if(target==='model'){setModelOpen(true);setMotionOpen(false);setManipulating(false);setSimulationRunning(false);setPlaybackRunning(false);}
    else if(target==='motion'){setMotionOpen(true);setModelOpen(false);}
    else {setAssistantOpen(true);requestAnimationFrame(()=>document.getElementById('model-prompt')?.focus());}
  }
  const [motionTask,setMotionTask]=useState<MotionTask>('move');
  const toolPanelOpen=useRef(motionOpen);toolPanelOpen.current=motionOpen;
  function openMotionTool(task:MotionTask,toggle=false){
    if(hostRef.current){hostRef.current.showTool(task,toggle);setMotionTask(task);setFocusedTool('motion');return;}
    if(motionTask!==task){setSimulationRunning(false);setPlaybackRunning(false);setManipulating(false);}
    const opening=!(toggle&&motionOpen&&motionTask===task);
    setMotionTask(task);setMotionOpen(opening);if(opening)setModelOpen(false);setFocusedTool('motion');
  }
  useEffect(()=>{if(!host)return;setModelOpen(host.visibleTools.includes('model'));setAssistantOpen(host.visibleTools.includes('assistant'));setMotionOpen(host.visibleTools.some(tool=>['move','simulate','test'].includes(tool)));},[host?.visibleTools.join(',')]);
  useEffect(()=>{
    if(!host?.active||!project)return;
    const onSelect=(event:Event)=>{
      const detail=(event as CustomEvent<{workspace:string;revision:string;instanceId:string}>).detail;
      if(detail.workspace!==project.workspace_id)return;
      if(viewer.current?.hasScene(detail.workspace,detail.revision))viewer.current.select(detail.instanceId);
      setSelection({revision:detail.revision,instance_id:detail.instanceId,entity_id:'part:0'});
    };
    const onVisibility=(event:Event)=>{
      const detail=(event as CustomEvent<{workspace:string;hidden:string[]}>).detail;
      if(detail.workspace!==project.workspace_id)return;
      applyVisibility(new Set(detail.hidden));
    };
    window.addEventListener('cadpad-canvas-select',onSelect);
    window.addEventListener('cadpad-canvas-visibility',onVisibility);
    return()=>{window.removeEventListener('cadpad-canvas-select',onSelect);window.removeEventListener('cadpad-canvas-visibility',onVisibility);};
  },[host?.active,project?.workspace_id,project?.revision]);
  const [advancedView,setAdvancedView]=useState(false);
  const [editing,setEditing]=useState(false);
  const menuAction=useRef<(action:string)=>void>(()=>{});
  const settingsDialog=useRef<HTMLDialogElement>(null);
  const [dimensions,setDimensions] = useState(false), [scope,setScope] = useState<Scope>('selected');
  const [motion,setMotion]=useState<MotionSnapshot|null>(null),[manipulating,setManipulating]=useState(false);
  const [interferenceResult,setInterferenceResult]=useState<InterferenceResult|null>(null),[checkingInterference,setCheckingInterference]=useState(false);
  const [playbackBusy,setPlaybackBusy]=useState(false),[playbackRunning,setPlaybackRunning]=useState(false);
  const playbackPending=useRef(false),timeline=useRef<{id:string;times:number[]}|null>(null),playbackClock=useRef({started:0,offset:0});
  const [simulationRunning,setSimulationRunning]=useState(false),[simulationBusy,setSimulationBusy]=useState(false);
  const [simulationDensity,setSimulationDensity]=useState(1000),[simulationFriction,setSimulationFriction]=useState(0.5),[simulationRestitution,setSimulationRestitution]=useState(0);
  const [simulationGravity,setSimulationGravity]=useState<[number,number,number]>([0,0,-9.81]),[simulationDynamic,setSimulationDynamic]=useState<Record<string,boolean>>({});
  const [motorForce,setMotorForce]=useState(10),[motorStiffness,setMotorStiffness]=useState(100),[motorDamping,setMotorDamping]=useState(10);
  const currentMotion=useRef<MotionSnapshot|null>(null),sendingMotion=useRef(false),targetFrame=useRef<number|null>(null);
  const pendingTargets=useRef(new Map<string,{workspace_id:string;revision:string;joint_id:string;coordinate:number;unit:string;check_collision:boolean}>());
  const settleTimers=useRef(new Map<string,number>());
  const interactiveThrottle=useRef<number|null>(null);
  const draftCoordinates=useRef(new Map<string,number>());
  const draftUiFrame=useRef<number|null>(null);
  const motionUiFrame=useRef<number|null>(null);
  const [,bumpDraft]=useState(0);
  const [busy,setBusy] = useState(false);
  const mutationBusy=useRef(false);
  const liveJob=useRef<Job|null>(null);
  const [hidden,updateHidden] = useState<Set<string>>(new Set());
  const hiddenIntent=useRef<Set<string>>(new Set());
  function setHidden(next:Set<string>){hiddenIntent.current=next;updateHidden(next);}
  const displayRetry = useRef(false);
  const [notice,setNotice] = useState('');
  const [selectionNotice,setSelectionNotice]=useState('');
  useEffect(()=>{
    if(!selectionNotice)return;
    const timer=window.setTimeout(()=>setSelectionNotice(''),5000);
    return()=>window.clearTimeout(timer);
  },[selectionNotice]);
  const [cleaningCache,setCleaningCache]=useState(false);
  const [workspaces,setWorkspaces]=useState<{workspace_id:string;name:string;running:number;model_parent?:string}[]>([]);
  const [external,setExternal] = useState<{changed:boolean;fingerprint:string;changes:Record<string,string>;conflicts?:string[];source_kind?:string}|null>(null);
  const [externalReviewOpen,setExternalReviewOpen]=useState(false);
  useEffect(()=>setExternalReviewOpen(false),[project?.workspace_id]);
  useEffect(()=>{if(!external?.changed)setExternalReviewOpen(false);},[external?.changed]);
  const [externalResolutions,setExternalResolutions]=useState<Record<string,'keep_cadpad'|'use_external'>>({});
  const reviewFingerprint=useRef('');
  const activeWorkspace = useRef<string|null>(null);
  const currentProject=useRef<Project|null>(null);
  /** Shared chrome chats stay on the project; agent turns and Explorer follow the focused canvas. */
  const displayProject=host?.chrome&&host.focusWorkspace&&host.focusWorkspace!==(project?.workspace_id||'')&&focusProject?.workspace_id===host.focusWorkspace?focusProject:project;
  const remoteSelection=host?.chrome&&host.focusWorkspace!==project?.workspace_id;
  const focusedSelection=remoteSelection&&displayProject&&host?.focusSelection&&host.focusSelection.workspace===displayProject.workspace_id&&host.focusSelection.revision===displayProject.revision?host.focusSelection:undefined;
  const composerReferences=remoteSelection?focusedSelection?.entities??[]:references;
  const composerOmitted=remoteSelection?focusedSelection?.omitted??new Set<string>():omitted;
  useEffect(()=>{
    if(!project||!hostRef.current?.onSelection)return;
    hostRef.current.onSelection({workspace:project.workspace_id,revision:project.revision,entities:references,omitted,
      capture:referenceView,toggleOmitted,clear:()=>{setSelection(null);viewer.current?.selectEntity(null);setOmitted(new Set());}});
  },[references,omitted,project?.workspace_id,project?.revision]);
  const chatScope=()=>{
    const projectWs=hostRef.current?.projectWorkspace,focusWs=hostRef.current?.focusWorkspace;
    if(hostRef.current?.chrome&&projectWs)return projectWs;
    return focusWs||projectWs||activeWorkspace.current;
  };
  const [settingsOpen,setSettingsOpen]=useState(false),[mcpSettings,setMcpSettings]=useState<McpSettings|null>(null);
  const [agentActivity,setAgentActivity]=useState('');
  const [chat,setChat]=useState<ChatMessage[]>([]),[agentTurn,setAgentTurn]=useState<AgentTurn|null>(null);
  const [prompt,updatePrompt]=useState('');
  const [chatList,setChatList]=useState<ChatSummary[]>([]),[activeChatId,setActiveChatId]=useState<string|null>(null),[chatBusy,setChatBusy]=useState(false);
  const [chatMessages,setChatMessages]=useState<Record<string,ChatMessage[]>>({});
  const [,bumpChatDrafts]=useState(0);
  const conversationRef=useRef<HTMLDivElement>(null);
  const chatHosts=useRef(new Map<string,HTMLElement>());
  const [visibleChatIds,setVisibleChatIds]=useState<string[]>([]);
  const chatMessagesRef=useRef(chatMessages);chatMessagesRef.current=chatMessages;
  const selectedChat=useRef<{workspace:string;id:string|null}|null>(null),chatDrafts=useRef(new Map<string,string>()),chatChangePending=useRef<string|null>(null);
  function chatHost(id:string){let node=chatHosts.current.get(id);if(!node){node=document.createElement('div');node.className='dock-content-host assistant-chat-host';chatHosts.current.set(id,node);}return node;}
  // Drop map entries only — DockArea Mount cleanup detaches the host node.
  function pruneChatHosts(open:Set<string>){
    for(const id of [...chatHosts.current.keys()])if(!open.has(id))chatHosts.current.delete(id);
  }
  function pruneChatDrafts(workspace:string,open:Set<string>){
    for(const key of [...chatDrafts.current.keys()]){
      const split=key.indexOf(':');if(split<0){chatDrafts.current.delete(key);continue;}
      const owner=key.slice(0,split),id=key.slice(split+1);
      if(owner!==workspace||id!=='legacy'&&id!=='new'&&!open.has(id))chatDrafts.current.delete(key);
    }
  }
  function draftFor(chatId:string){
    const selected=selectedChat.current;
    if(selected&&chatId===(selected.id??'new'))return prompt;
    return chatDrafts.current.get(`${selected?.workspace||chatScope()||''}:${chatId}`)||'';
  }
  function setDraft(chatId:string,value:string){
    const workspace=selectedChat.current?.workspace||chatScope();
    if(!workspace)return;
    if(chatId===(selectedChat.current?.id??'new')||chatId===activeChatId){setPrompt(value);return;}
    chatDrafts.current.set(`${workspace}:${chatId}`,value);bumpChatDrafts(version=>version+1);
  }
  function setPrompt(next:React.SetStateAction<string>){
    const selected=selectedChat.current;
    updatePrompt(previous=>{
      const value=typeof next==='function'?next(previous):next;
      if(selected)chatDrafts.current.set(`${selected.workspace}:${selected.id??'legacy'}`,value);
      return value;
    });
  }
  const pendingAgentStarts=useRef(new Map<string,PendingAgentStart>());
  const [agentStarts,setAgentStarts]=useState<Record<string,'starting'|'cancelling'>>({});
  const agentScope=host?.chrome?(host.projectWorkspace||host.focusWorkspace):project?.workspace_id;
  const agentSending=!!agentScope&&!!agentStarts[agentScope];
  const agentStartCancelling=!!agentScope&&agentStarts[agentScope]==='cancelling';
  // Chat loads and live turn updates are independent; streaming must not drop a
  // just-fetched user message that refreshChat is about to commit.
  const chatEpoch=useRef(0);
  const [providerOpen,setProviderOpen]=useState(false),[providerSettings,setProviderSettings]=useState<AgentSettings|null>(null),[agentModels,setAgentModels]=useState<ModelChoice[]|null>(null);
  const [agentBackend,setAgentBackend]=useState('claude_cli');
  const [modelId,setModelId]=useState(''),[apiKey,setApiKey]=useState(''),[savingProvider,setSavingProvider]=useState(false);
  const liveTurn=useRef<AgentTurn|null>(null);
  const pendingTurnChat=useRef(new Map<string,string>());
  const settledTurns=useRef(new Set<string>());
  const [pendingChatIds,setPendingChatIds]=useState<string[]>([]);
  const [landedChat,setLandedChat]=useState<{id:string;seq:number}|null>(null);
  function publishPending(){
    const ids=[...new Set(pendingTurnChat.current.values())];
    setPendingChatIds(current=>current.length===ids.length&&current.every((id,index)=>id===ids[index])?current:ids);
  }
  function notePendingChat(chatId:string,turnId?:string){
    if(!chatId||turnId&&settledTurns.current.has(turnId))return;
    if(turnId){pendingTurnChat.current.delete(`local:${chatId}`);pendingTurnChat.current.set(turnId,chatId);}
    else pendingTurnChat.current.set(`local:${chatId}`,chatId);
    publishPending();
  }
  function noteSettledChat(chatId:string,turnId:string){
    if(!chatId||!turnId||settledTurns.current.has(turnId))return;
    settledTurns.current.add(turnId);
    pendingTurnChat.current.delete(turnId);
    pendingTurnChat.current.delete(`local:${chatId}`);
    publishPending();
    setLandedChat({id:chatId,seq:settledTurns.current.size});
  }
  function dropPendingStart(chatId:string){
    if(!pendingTurnChat.current.delete(`local:${chatId}`))return;
    publishPending();
  }
  function resetChatAttention(){
    pendingTurnChat.current.clear();settledTurns.current.clear();setPendingChatIds([]);setLandedChat(null);
  }
  function showTurn(turn:AgentTurn|null){
    const old=liveTurn.current;
    if(turn&&old?.id===turn.id){
      if(!['running','cancelling'].includes(old.state)&&['running','cancelling'].includes(turn.state))return;
      if(old.state==='cancelling'&&turn.state==='running')turn={...turn,state:'cancelling',activity:'Stopping…'};
    }
    liveTurn.current=turn;setAgentTurn(turn);
  }
  function acceptChat(workspace:string,result:{messages:ChatMessage[];turn:AgentTurn|null;active_chat_id?:string;chats?:ChatSummary[]}){
    if(!result||!Array.isArray(result.messages))throw new Error('The conversation reply is invalid. Your project is unchanged.');
    const id=result.active_chat_id??null,previous=selectedChat.current;
    if(!previous||previous.workspace!==workspace||previous.id!==id)updatePrompt(chatDrafts.current.get(`${workspace}:${id??'legacy'}`)||'');
    selectedChat.current={workspace,id};setActiveChatId(id);setChatList(result.chats??[]);setChat(result.messages);if(id)setChatMessages(current=>({...current,[id]:result.messages}));showTurn(result.turn);
    const live=result.turn;
    if(live?.chat_id&&['running','cancelling'].includes(live.state))notePendingChat(live.chat_id,live.id);
    else if(live?.chat_id&&live.id&&['succeeded','failed','cancelled'].includes(live.state)&&(pendingTurnChat.current.has(live.id)||[...pendingTurnChat.current.values()].includes(live.chat_id)))noteSettledChat(live.chat_id,live.id);
  }
  async function refreshChat(workspace:string){
    if(chatChangePending.current===workspace)return;
    const epoch=++chatEpoch.current;
    const result=await invoke<{messages:ChatMessage[];turn:AgentTurn|null;active_chat_id?:string;chats?:ChatSummary[]}>('agent_conversation',{workspace});
    if(chatScope()===workspace&&chatEpoch.current===epoch)acceptChat(workspace,result);
  }
  async function changeChat(action:'new'|'select'|'remove'|'restore',id?:string):Promise<boolean>{
    const workspace=chatScope();if(!workspace||chatChangePending.current||pendingAgentStarts.current.has(workspace))return false;
    chatChangePending.current=workspace;setChatBusy(true);chatEpoch.current++;setError('');
    try{
      const result=await invoke<{messages:ChatMessage[];turn:AgentTurn|null;active_chat_id:string;chats:ChatSummary[]}>('agent_chat',{workspace,action,chatId:id??null});
      if(chatScope()!==workspace)return false;
      acceptChat(workspace,result);if(action==='new'){setPrompt('');setAttachedImage(null);}setOmitted(new Set());requestAnimationFrame(()=>document.getElementById('model-prompt')?.focus());return true;
    }catch(error){if(chatScope()===workspace)setError(message(error));return false;}
    finally{chatChangePending.current=null;setChatBusy(false);}
  }
  function starting(workspace:string,state?:'starting'|'cancelling'){
    setAgentStarts(current=>{const next={...current};if(state)next[workspace]=state;else delete next[workspace];return next;});
  }
  async function cancelAcceptedStart(pending:PendingAgentStart){
    const turn=pending.turn;if(!turn)return;
    const previous=liveTurn.current?.id===turn.id?liveTurn.current:turn;
    if(liveTurn.current?.id===turn.id&&!['running','cancelling'].includes(liveTurn.current.state))return;
    if(activeWorkspace.current===pending.workspace&&liveTurn.current?.id===turn.id)showTurn({...liveTurn.current,state:'cancelling',activity:'Stopping…'});
    try{
      const stopped=await(pending.stopping??=invoke<AgentTurn>('agent_stop',{turnId:turn.id}));
      if(activeWorkspace.current===pending.workspace&&liveTurn.current?.id===turn.id)showTurn(stopped);
    }catch(error){
      if(liveTurn.current?.id===turn.id&&!['running','cancelling'].includes(liveTurn.current.state))return;
      if(activeWorkspace.current===pending.workspace&&liveTurn.current?.id===turn.id&&liveTurn.current.state==='cancelling'){const restored={...liveTurn.current,state:previous.state,activity:previous.activity};liveTurn.current=restored;setAgentTurn(restored);}
      throw error;
    }
  }
  const [editingMessage,setEditingMessage]=useState<{chatId:string;index:number;text:string;revision?:string}|null>(null);
  const [editingMessageBusy,setEditingMessageBusy]=useState(false);
  async function submitMessageEdit(){
    const edit=editingMessage,current=currentProject.current;
    if(!edit||!current||!edit.text.trim()||editingMessageBusy)return;
    setEditingMessageBusy(true);setError('');
    try{
      if(edit.revision&&edit.revision!==current.revision){
        let job=await invoke<Job>('restore_checkpoint',{workspace:current.workspace_id,revision:current.revision,target:edit.revision});
        while(['queued','running'].includes(job.state)){
          await new Promise(resolve=>window.setTimeout(resolve,200));
          job=await invoke<Job>('job',{workspace:current.workspace_id,jobId:job.id});
        }
        if(job.state!=='succeeded')throw new Error('Could not restore the model. Your conversation was kept.');
        adopt(await invoke<Project>('project',{workspace:current.workspace_id}));
      }
      await invoke('agent_chat',{workspace:current.workspace_id,action:`rewind:${edit.index}`,chatId:edit.chatId});
      await refreshChat(current.workspace_id);
      setEditingMessage(null);
      await sendPrompt(edit.text,edit.chatId);
    }catch(error){setError(message(error));}finally{setEditingMessageBusy(false);}
  }
  async function sendPrompt(submission?:string,chatId?:string){
    const current=displayProject||currentProject.current;
    // Chrome keeps one project chat book mounted across model canvases.
    const workspace=chatScope();
    const targetChat=chatId??selectedChat.current?.id??activeChatId??undefined;
    const submitted=submission??(targetChat?draftFor(targetChat):prompt);
    if(chatChangePending.current||!current||!workspace||!submitted.trim()||pendingAgentStarts.current.has(workspace))return;
    if(liveTurn.current?.workspace_id===workspace&&['running','cancelling'].includes(liveTurn.current.state))return;
    const previousId=liveTurn.current?.id;
    const pending:PendingAgentStart={workspace,cancelRequested:false};
    const pendingTab=targetChat&&targetChat!=='new'?targetChat:'new';
    pendingAgentStarts.current.set(workspace,pending);starting(workspace,'starting');notePendingChat(pendingTab);chatEpoch.current++;setError('');setNotice('');
    // Show the outbound message immediately; refreshChat reconciles with the server.
    if(chatScope()===workspace){
      const optimistic={role:'user',text:submitted,image:attachedImage?.data};
      const base=!targetChat||targetChat===activeChatId?chat:chatMessages[targetChat]||[];
      const next=[...base,optimistic];
      if(!targetChat||targetChat===activeChatId)setChat(next);
      if(targetChat)setChatMessages(current=>({...current,[targetChat]:next}));
    }
    try{
      const candidates=remoteSelection?focusedSelection?.entities??[]:referencesNow.current;
      const attached=current.revision===candidates[0]?.revision?candidates.filter(item=>!composerOmitted.has(entityKey(item))):[];
      const image=attachedImage?.data??(attached.length?(remoteSelection?focusedSelection?.capture()??null:referenceView()):null);
      const turn=await invoke<AgentTurn>('agent_start',{workspace,...(current.workspace_id!==workspace?{modelWorkspace:current.workspace_id}:{}),prompt:submitted,references:attached,image,...(targetChat&&targetChat!=='new'?{chatId:targetChat}:{})});
      if(turn.workspace_id!==workspace)throw new Error('Agent reply belongs to another workspace');
      pending.turn=turn;
      dropPendingStart(pendingTab);
      if(turn.chat_id){if(['succeeded','failed','cancelled'].includes(turn.state))noteSettledChat(turn.chat_id,turn.id);else notePendingChat(turn.chat_id,turn.id);}
      if(chatScope()===workspace&&(!liveTurn.current||liveTurn.current.id===turn.id||liveTurn.current.id===previousId))showTurn(turn);
      if(pending.cancelRequested)await cancelAcceptedStart(pending);
      if(chatScope()===workspace){if(!pending.cancelRequested){if(targetChat)setDraft(targetChat,'');else setPrompt(value=>value===submitted?'':value);setAttachedImage(null);if(remoteSelection)focusedSelection?.clear();else{setSelection(null);viewer.current?.selectEntity(null);setOmitted(new Set());}}await refreshChat(workspace);}
    }catch(error){
      dropPendingStart(pendingTab);
      if(pending.cancelRequested&&!pending.turn){if(chatScope()===workspace)void refreshChat(workspace).catch(()=>{});return;}
      if(chatScope()===workspace){const failure=error as {code?:string};if(failure.code==='LLM_NOT_CONFIGURED')setProviderOpen(true);setError(message(error));void refreshChat(workspace).catch(()=>{});}
    }
    finally{if(pendingAgentStarts.current.get(workspace)===pending){pendingAgentStarts.current.delete(workspace);starting(workspace);}}
  }
  /** The live view with references highlighted and lettered, bounded for the model's context. */
  function referenceView():string|null{
    for(const [width,height] of [[1024,768],[768,576]]){
      try{const image=viewer.current?.capture({width,height,dimensions:false}).image;if(image&&image.length<=MAX_ATTACHED_IMAGE)return image;}catch{return null;}
    }
    return null;
  }
  function imageDrag(event:React.DragEvent,phase:'enter'|'over'|'leave'|'drop'){
    if(![...event.dataTransfer.types].includes('Files'))return;
    event.preventDefault();
    if(phase==='over'){event.dataTransfer.dropEffect='copy';return;}
    if(phase==='enter'){setImageDropDepth(depth=>depth+1);return;}
    if(phase==='leave'){setImageDropDepth(depth=>Math.max(0,depth-1));return;}
    setImageDropDepth(0);
    const file=[...event.dataTransfer.files].find(item=>item.type.startsWith('image/'));
    if(!file){setError('Drop an image file to attach it to your next message.');return;}
    void encodeReferenceImage(file).then(image=>{setAttachedImage(image);setPanel('assistant');setError('');}).catch(error=>setError(message(error)));
  }
  function referenceChips():(ChatReference&{entity:Entity;omitted:boolean})[]{
    const parts=displayProject?.model.parts||[];let next=0;
    const included=composerReferences.filter(item=>!composerOmitted.has(entityKey(item))).length;
    return composerReferences.map(entity=>{
      const part=parts.find(item=>item.instance_id===entity.instance_id),left=composerOmitted.has(entityKey(entity));
      const kind=entity.entity_id.split(':')[0],features=Object.entries(part?.features||{}).filter(([,ids])=>ids.includes(entity.entity_id)).map(([name])=>name);
      const letter=left||included<2?'':referenceLetter(next++);
      return {entity,omitted:left,letter,instance_id:entity.instance_id,entity_id:entity.entity_id,part:part?.name||entity.instance_id,label:features.length?features.join(', '):kind==='part'?'Object':humanLabel(kind),file:part?.source?.file,line:part?.source?.line};
    });
  }
  function toggleOmitted(entity:Entity){setOmitted(current=>{const next=new Set(current),key=entityKey(entity);next.has(key)?next.delete(key):next.add(key);return next;});}
  async function stopAgent(){
    const workspace=activeWorkspace.current;if(!workspace)return;
    const pending=pendingAgentStarts.current.get(workspace);
    if(pending){pending.cancelRequested=true;starting(workspace,'cancelling');if(pending.turn){try{await cancelAcceptedStart(pending);}catch(error){if(activeWorkspace.current===workspace)setError(message(error));}}return;}
    const previous=liveTurn.current;if(!previous||previous.workspace_id!==workspace||!['running','cancelling'].includes(previous.state))return;
    showTurn({...previous,state:'cancelling',activity:'Stopping…'});
    try{const turn=await invoke<AgentTurn>('agent_stop',{turnId:previous.id});if(activeWorkspace.current===workspace&&liveTurn.current?.id===previous.id){showTurn(turn);if(['succeeded','failed','cancelled'].includes(turn.state))await refreshChat(workspace);}}
    catch(error){if(activeWorkspace.current===workspace&&liveTurn.current?.id===previous.id&&liveTurn.current.state==='cancelling'){const restored={...liveTurn.current,state:previous.state,activity:previous.activity};liveTurn.current=restored;setAgentTurn(restored);setError(message(error));}}
  }
  async function saveProvider(){
    setSavingProvider(true);setError('');
    try{
      const settings=await invoke<AgentSettings>('configure_agent',{backend:agentBackend,model:modelId,apiKey:apiKey||null});
      setProviderSettings(settings);setAgentBackend(settings.backend||settings.provider);setModelId(settings.model||'');setApiKey('');
      setProviderOpen(!settings.configured);
      setNotice(settings.configured?'Agent settings saved. Send your request to start modeling.':settings.credential_storage==='local_cli'?'CLI selected. Sign in with that CLI if Cadpad still shows it as unavailable.':'Model saved. Add your API key to start modeling.');
    }
    catch(e){setError(message(e));}finally{setSavingProvider(false);}
  }
  async function saveAgentArtifact(artifact:{handle:string;format:string;revision:string}){
    if(!project)return;const workspace=project.workspace_id;
    try{await invoke('export',{workspace,revision:artifact.revision,format:artifact.format,handle:artifact.handle});}
    catch(e){if(activeWorkspace.current===workspace)setError(message(e));}
  }
  function receiveMotion(snapshot:MotionSnapshot){
    const project=currentProject.current,old=currentMotion.current;
    if(!project||activeWorkspace.current!==snapshot.workspace_id||project.workspace_id!==snapshot.workspace_id||project.revision!==snapshot.revision)return;
    if(old?.workspace_id===snapshot.workspace_id&&old.revision===snapshot.revision&&old.pose_version>snapshot.pose_version)return;
    let clearedDraft=false;for(const joint of snapshot.joints){const draft=draftCoordinates.current.get(joint.id);if(draft!==undefined&&Math.abs(draft-joint.coordinate)<1e-12){draftCoordinates.current.delete(joint.id);clearedDraft=true;}}
    if(clearedDraft){
      if(draftUiFrame.current!==null){cancelAnimationFrame(draftUiFrame.current);draftUiFrame.current=null;}
      bumpDraft(value=>value+1);
    }
    currentMotion.current=snapshot;viewer.current?.setMotion(snapshot);
    // During live drag, keep WebGL immediate and coalesce inspector React work off the pointer path.
    const dragging=[...pendingTargets.current.values()].some(target=>!target.check_collision);
    if(dragging){
      if(motionUiFrame.current===null)motionUiFrame.current=requestAnimationFrame(()=>{motionUiFrame.current=null;if(currentMotion.current)setMotion(currentMotion.current);});
    }else{
      if(motionUiFrame.current!==null){cancelAnimationFrame(motionUiFrame.current);motionUiFrame.current=null;}
      setMotion(snapshot);
    }
    if(snapshot.actor==='local-user'){const pending=[...pendingTargets.current.values()].at(-1);if(pending&&pending.workspace_id===snapshot.workspace_id&&pending.revision===snapshot.revision)viewer.current?.previewCoordinate(pending.joint_id,pending.coordinate);}
  }
  async function checkInterference(){
    const current=currentProject.current,snapshot=currentMotion.current;if(!current||!snapshot)return;
    const parts=current.model.parts.filter(part=>!hidden.has(part.instance_id));const pairs:[string,string][]=[];
    for(let i=0;i<parts.length;i++)for(let j=i+1;j<parts.length;j++)if(!selection||[parts[i].instance_id,parts[j].instance_id].includes(selection.instance_id))pairs.push([parts[i].instance_id,parts[j].instance_id]);
    if(!pairs.length||pairs.length>64){setNotice('Select a part and show fewer parts to check up to 64 pairs.');return;}
    setCheckingInterference(true);setError('');
    try{const result=await invoke<InterferenceResult>('interference',{request:{workspace_id:current.workspace_id,revision:current.revision,pose_version:snapshot.pose_version,pairs,exact:true}});if(activeWorkspace.current===current.workspace_id&&currentMotion.current?.revision===result.revision&&currentMotion.current.pose_version===result.pose_version)setInterferenceResult(result);}
    catch(error){if(activeWorkspace.current===current.workspace_id)setError(message(error));}finally{setCheckingInterference(false);}
  }
  async function refreshMotion(){const current=currentProject.current;if(current)receiveMotion(await invoke<MotionSnapshot>('motion',{workspace:current.workspace_id,revision:current.revision}));}
  async function simulationAction(command:unknown,expected?:MotionSnapshot){
    const current=expected??currentMotion.current;if(!current||sendingMotion.current||current.workspace_id!==activeWorkspace.current)return;
    sendingMotion.current=true;setSimulationBusy(true);
    try{receiveMotion(await invoke<MotionSnapshot>('simulation',{request:{workspace_id:current.workspace_id,revision:current.revision,expected_pose_version:current.pose_version,command}}));}
    catch(error){setSimulationRunning(false);if(activeWorkspace.current===current.workspace_id){setError(message(error));await refreshMotion();}}
    finally{sendingMotion.current=false;setSimulationBusy(false);scheduleTarget();}
  }
  async function startSimulation(){
    const current=currentProject.current,snapshot=currentMotion.current;if(!current||!snapshot)return;
    const children=new Set(snapshot.joints.map(joint=>joint.child));
    const bodies=Object.fromEntries(current.model.parts.map(part=>[part.instance_id,(simulationDynamic[part.instance_id]??children.has(part.instance_id))?{type:'dynamic',mass:{type:'density',density_kg_m3:simulationDensity},friction:simulationFriction,restitution:simulationRestitution}:{type:'fixed',friction:simulationFriction,restitution:simulationRestitution}]));
    pendingTargets.current.clear();await simulationAction({action:'start',definition:{gravity_m_s2:simulationGravity,bodies}});
    if(activeWorkspace.current===current.workspace_id&&currentMotion.current?.revision===snapshot.revision&&currentMotion.current?.mode==='simulate'&&!currentMotion.current.playback)setSimulationRunning(true);
  }
  useEffect(()=>{if(!simulationRunning||motion?.mode!=='simulate'||motion.playback)return;const timer=setInterval(()=>{if(!sendingMotion.current&&!pendingTargets.current.size)void simulationAction({action:'advance',steps:2});},1000/60);return()=>clearInterval(timer);},[simulationRunning,motion?.mode,motion?.workspace_id,motion?.revision,motion?.playback?.recording_id]);
  async function playbackAction(command:{action:string;[key:string]:unknown}){
    const workspace=activeWorkspace.current;if(!workspace||playbackPending.current)return;
    playbackPending.current=true;setPlaybackBusy(true);setSimulationRunning(false);pendingTargets.current.clear();
    if(command.action!=='select')setPlaybackRunning(false);
    try{
      while(sendingMotion.current){await new Promise(resolve=>setTimeout(resolve,10));if(activeWorkspace.current!==workspace)return;}
      const current=currentMotion.current;if(!current||current.workspace_id!==workspace)return;
      sendingMotion.current=true;
      try{
        const result=await invoke<PlaybackReply>('playback',{request:{workspace_id:workspace,revision:current.revision,expected_pose_version:current.pose_version,command}});
        if(activeWorkspace.current!==workspace)return;
        receiveMotion(result.snapshot);
        if(result.frame_times_s&&result.snapshot.playback)timeline.current={id:result.snapshot.playback.recording_id,times:result.frame_times_s};
        if(command.action==='finish_recording'||command.action==='delete'){
          const next=await invoke<Project>('project',{workspace});
          if(activeWorkspace.current===workspace&&currentProject.current?.revision===next.revision){currentProject.current=next;setProject(next);}
        }
      }finally{sendingMotion.current=false;}
    }catch(error){setPlaybackRunning(false);if(activeWorkspace.current===workspace){setError(message(error));await refreshMotion();}}
    finally{playbackPending.current=false;setPlaybackBusy(false);scheduleTarget();}
  }
  function queueJoint(joint:string,coordinate:number,unit:string,check_collision=true){
    const current=currentProject.current;if(!current||currentMotion.current?.playback)return;
    draftCoordinates.current.set(joint,coordinate);
    if(check_collision){
      if(draftUiFrame.current!==null){cancelAnimationFrame(draftUiFrame.current);draftUiFrame.current=null;}
      bumpDraft(value=>value+1);
    }else if(draftUiFrame.current===null){
      draftUiFrame.current=requestAnimationFrame(()=>{draftUiFrame.current=null;bumpDraft(value=>value+1);});
    }
    pendingTargets.current.set(joint,{workspace_id:current.workspace_id,revision:current.revision,joint_id:joint,coordinate,unit,check_collision});
    const timers=settleTimers.current;
    if(!check_collision){
      if(timers.has(joint))window.clearTimeout(timers.get(joint));
      timers.set(joint,window.setTimeout(()=>{timers.delete(joint);queueJoint(joint,draftCoordinates.current.get(joint)??coordinate,unit,true);},120));
      const live=currentMotion.current;
      // Open-tree local previews already move the part; throttle authoritative writes so IPC/React do not hitch the pointer.
      if(live&&!live.constraints&&live.mode!=='simulate'&&!live.collision){
        if(interactiveThrottle.current===null)interactiveThrottle.current=window.setTimeout(()=>{interactiveThrottle.current=null;scheduleTarget();},32);
        return;
      }
    }else{
      if(timers.has(joint)){window.clearTimeout(timers.get(joint));timers.delete(joint);}
      if(interactiveThrottle.current!==null){window.clearTimeout(interactiveThrottle.current);interactiveThrottle.current=null;}
    }
    scheduleTarget();
  }
  function scheduleTarget(){
    if(sendingMotion.current||targetFrame.current!==null||!pendingTargets.current.size)return;
    targetFrame.current=requestAnimationFrame(()=>{targetFrame.current=null;void sendTarget().catch(error=>setError(message(error)));});
  }
  async function sendTarget(){
    if(sendingMotion.current||!pendingTargets.current.size)return;
    sendingMotion.current=true;
    // Prefer interactive previews so the viewport stays ahead of collision commits.
    const interactive=[...pendingTargets.current.entries()].find(([,target])=>!target.check_collision);
    const [key,target]=interactive??pendingTargets.current.entries().next().value!;pendingTargets.current.delete(key);
    try{
      const current=currentMotion.current;
      if(!current||current.workspace_id!==target.workspace_id||current.revision!==target.revision||activeWorkspace.current!==target.workspace_id)return;
      const joint=current.joints.find(entry=>entry.id===target.joint_id);
      if(joint?.coordinate===target.coordinate&&(!target.check_collision||!current.collision||current.collision_checked))return;
      if(current.playback)return;
      if(current.mode==='simulate'){
        receiveMotion(await invoke<MotionSnapshot>('simulation',{request:{workspace_id:current.workspace_id,revision:current.revision,expected_pose_version:current.pose_version,command:{action:'motor',target:{joint_id:target.joint_id,target:target.coordinate,unit:target.unit,stiffness:motorStiffness,damping:motorDamping,max_force:motorForce}}}}));return;
      }
      try{const result=await invoke<{snapshot:MotionSnapshot;collision?:{blocked:boolean;instances:[string,string]};applied?:number}> ('joint_coordinate',{request:{workspace_id:target.workspace_id,revision:target.revision,joint_id:target.joint_id,coordinate:target.coordinate,unit:target.unit,check_collision:target.check_collision,expected_pose_version:current.pose_version}});if(typeof result.applied==='number'){draftCoordinates.current.set(target.joint_id,result.applied);bumpDraft(value=>value+1);}receiveMotion(result.snapshot);if(result.collision?.blocked&&activeWorkspace.current===target.workspace_id)setNotice(`Motion stopped between ${result.collision.instances.join(' and ')} using conservative proxies.`);}
      catch(error){
        const text=message(error);
        const soft=!target.check_collision&&/INFEASIBLE_MECHANISM|SINGULAR_MECHANISM|CONSTRAINT_PATH_LIMIT/.test(text);
        // Interactive previews may skip singular/infeasible jumps; keep the last good pose and continue.
        if(soft){const live=currentMotion.current?.joints.find(entry=>entry.id===target.joint_id)?.coordinate;if(live!==undefined){draftCoordinates.current.set(target.joint_id,live);bumpDraft(value=>value+1);}}
        else{pendingTargets.current.clear();draftCoordinates.current.clear();if(activeWorkspace.current===target.workspace_id){setError(text);await refreshMotion();}}
      }
    }finally{sendingMotion.current=false;scheduleTarget();}
  }
  function adopt(next:Project) { if(!sessionAlive.current)return; if(hostRef.current&&next.workspace_id!==hostRef.current.workspaceId){hostRef.current.onOpen(next);return;} if(!next||typeof next.workspace_id!=='string'||typeof next.revision!=='string'||!Array.isArray(next.model?.parts)||!Array.isArray(next.activity)||!next.parameters||typeof next.parameters!=='object')throw new Error('The workspace reply is invalid. Open another project or rebuild this workspace; existing projects are unchanged.'); const switching=currentProject.current?.workspace_id!==next.workspace_id;const rebuilding=currentProject.current?.revision!==next.revision;activeWorkspace.current=next.workspace_id;currentProject.current=next;setProject(next);hostRef.current?.onProject(next);if(switching||rebuilding){for(const timer of settleTimers.current.values())window.clearTimeout(timer);settleTimers.current.clear();if(interactiveThrottle.current!==null){window.clearTimeout(interactiveThrottle.current);interactiveThrottle.current=null;}if(draftUiFrame.current!==null){cancelAnimationFrame(draftUiFrame.current);draftUiFrame.current=null;}if(motionUiFrame.current!==null){cancelAnimationFrame(motionUiFrame.current);motionUiFrame.current=null;}pendingTargets.current.clear();draftCoordinates.current.clear();currentMotion.current=null;setManipulating(false);setSimulationRunning(false);setSimulationDynamic({});setPlaybackRunning(false);timeline.current=null;}receiveMotion(next.motion);if(switching)setJob(next.activity.find(job=>['queued','running'].includes(job.state))||next.activity.at(-1)||null); }
  useEffect(()=>{invoke<{workspace_id:string;name:string;running:number;model_parent?:string}[]>('workspaces').then(setWorkspaces).catch(e=>setError(message(e)));},[project?.workspace_id,project?.name,job?.state]);
  async function switchWorkspace(workspace:string){
    if(hostRef.current){hostRef.current.onOpenWorkspace(workspace);return;}
    // Invalidate callbacks from the previous tab before awaiting IPC.
    activeWorkspace.current=workspace;setSimulationRunning(false);setPlaybackRunning(false);setJob(null);setSelection(null);setExternal(null);setError('');
    try{
      const next=await invoke<Project>('switch_project',{workspace});
      if(activeWorkspace.current!==workspace)return;
      adopt(next);const active=next.activity.find(job=>['queued','running'].includes(job.state))||next.activity.at(-1)||null;liveJob.current=active;setJob(active);
    }catch(e){if(activeWorkspace.current===workspace)setError(message(e));}
  }

  useEffect(()=>{
    const channel=new Channel<{workspace_id:string;kind:string;job?:Job;tool?:string}>();
    channel.onmessage=event=>{
      const current=currentProject.current;
      const focusWs=hostRef.current?.chrome?hostRef.current.focusWorkspace:'';
      if(focusWs&&event.workspace_id===focusWs&&focusWs!==current?.workspace_id&&(event.kind==='refresh'||event.job&&!['queued','running'].includes(event.job.state))){
        invoke<Project>('project',{workspace:focusWs}).then(next=>{if(hostRef.current?.focusWorkspace===focusWs)setFocusProject(next);}).catch(()=>{});
      }
      if(!current || event.workspace_id!==current.workspace_id&&event.kind!=='refresh')return;
      if(event.job){liveJob.current=event.job;setJob(event.job);}
      if(event.kind==='tool_started')setAgentActivity('Agent is working…');
      if(event.kind==='tool_succeeded'||event.kind==='tool_failed'||event.kind==='refresh')setAgentActivity('');
      if(event.kind==='refresh'||event.kind==='recordings_changed'||event.kind==='motion_tests_changed'||event.job&&!['queued','running'].includes(event.job.state)){
        invoke<Project>('project',{workspace:current.workspace_id}).then(next=>{if(activeWorkspace.current===next.workspace_id)adopt(next);}).catch(e=>setError(message(e)));
      }
    };
    return connectNativeStream('connect_activity',channel,e=>setError(message(e)));
  },[]);
  useEffect(()=>{
    const channel=messageChannel<{id:string;workspace_id:string;revision:string;command:string;arguments:Parameters<CadViewport['capture']>[0]&{visible:boolean;scope:Scope;motion?:MotionSnapshot}}>(!!hostRef.current);
    const token=hostRef.current?hostRef.current.connectView(channel):invoke<string>('connect_view',{channel});
    channel.onmessage=async request=>{
      let reply: {data?:unknown;error?:{code:string;message:string}};
      const current=currentProject.current;
      try{
        if(!current || activeWorkspace.current!==request.workspace_id || current.workspace_id!==request.workspace_id)throw {code:'APP_UNAVAILABLE',message:'Open the requested workspace in Cadpad to capture or change its viewport'};
        if(current.revision!==request.revision)throw {code:'STALE_ENTITY',message:'The requested revision is not displayed'};
        if(request.command==='capture'){if(!viewer.current?.hasScene(request.workspace_id,request.revision))throw {code:'VIEW_NOT_READY',message:'The requested scene has no displayed geometry yet. Build or restore its display, then retry.'};if(request.arguments?.motion)receiveMotion(request.arguments.motion);reply={data:viewer.current.capture(request.arguments)};}
        else if(request.command==='dimensions'){
          viewer.current?.setDimensions(request.arguments!.visible,request.arguments!.scope);setDimensions(request.arguments!.visible);setScope(request.arguments!.scope);
          reply={data:{visible:request.arguments!.visible,scope:request.arguments!.scope,revision:current.revision,workspace_id:current.workspace_id}};
        }else throw {code:'UNSUPPORTED_OPERATION',message:'Unknown viewport request'};
      }catch(error){const failure=error as {code?:string;message?:string};reply={error:{code:failure.code||'APP_UNAVAILABLE',message:failure.message||String(error)}};}
      const viewerToken=await token.catch(()=>null);
      if(viewerToken)invoke('complete_view',{token:viewerToken,requestId:request.id,reply}).catch(()=>{});
    };
    token.catch(e=>setError(message(e)));
    return()=>{channel.onmessage=()=>{};if('id' in channel)void token.then(()=>invoke('disconnect_stream',{subscriptionId:channel.id})).catch(()=>{});};
  },[]);
  useEffect(()=>{
    const channel=new Channel<MotionSnapshot&{kind?:string}>();
    channel.onmessage=snapshot=>{if(snapshot.kind==='refresh'){void refreshMotion().catch(e=>setError(message(e)));return;}receiveMotion(snapshot);};
    return connectNativeStream('connect_motion',channel,e=>setError(message(e)));
  },[]);
  useEffect(()=>{
    if(!settingsOpen||!providerOpen)return;
    let active=true;const refresh=()=>invoke<McpSettings>('mcp_settings').then(result=>{if(active)setMcpSettings(result);}).catch(e=>{if(active)setError(message(e));});
    void refresh();const timer=setInterval(refresh,2000);return()=>{active=false;clearInterval(timer);};
  },[settingsOpen,providerOpen]);
  useEffect(()=>{
    if(!host?.chrome){setFocusProject(null);return;}
    const target=host.focusWorkspace;
    if(!target||target===host.workspaceId){setFocusProject(null);return;}
    let cancelled=false;
    invoke<Project>('project',{workspace:target}).then(next=>{
      if(!cancelled&&hostRef.current?.focusWorkspace===target)setFocusProject(next);
    }).catch(error=>{if(!cancelled&&hostRef.current?.chrome)setError(message(error));});
    return()=>{cancelled=true;};
  },[host?.chrome,host?.focusWorkspace,host?.workspaceId]);
  const chatWorkspace=host?.chrome?(host.projectWorkspace||host.focusWorkspace):project?.workspace_id;
  useEffect(()=>{
    if(!chatWorkspace)return;
    pruneChatHosts(new Set());
    resetChatAttention();
    setChat([]);setChatList([]);setActiveChatId(null);setChatMessages({});setAgentActivity('');setAttachedImage(null);setImageDropDepth(0);showTurn(null);
    refreshChat(chatWorkspace).catch(e=>setError(message(e)));
  },[chatWorkspace]);
  useEffect(()=>{
    if(!chatWorkspace)return;
    const open=new Set(chatTabIds(chatList,activeChatId));
    pruneChatHosts(open);
    pruneChatDrafts(chatWorkspace,open);
  },[chatWorkspace,chatList,activeChatId]);
  const visibleChatKey=JSON.stringify(visibleChatIds);
  useEffect(()=>{
    if(!chatWorkspace)return;
    // Retain message bodies only for the selected chat and visible split panes.
    const visible=new Set(visibleChatIds);
    if(activeChatId)visible.add(activeChatId);
    setChatMessages(current=>{
      const next=Object.fromEntries(Object.entries(current).filter(([id])=>visible.has(id)));
      return Object.keys(next).length===Object.keys(current).length?current:next;
    });
    let cancelled=false;
    for(const id of visible){
      if(!id||id==='new'||id===activeChatId||chatMessagesRef.current[id])continue;
      void invoke<{messages:ChatMessage[]}>('agent_conversation',{workspace:chatWorkspace,chatId:id}).then(result=>{
        if(cancelled||chatScope()!==chatWorkspace||!Array.isArray(result.messages))return;
        setChatMessages(current=>current[id]?current:{...current,[id]:result.messages});
      }).catch(()=>{});
    }
    return()=>{cancelled=true;};
  },[chatWorkspace,visibleChatKey,activeChatId]);
  useEffect(()=>{
    if(!providerOpen)return;
    invoke<AgentSettings>('agent_settings').then(settings=>{setProviderSettings(settings);setAgentBackend(settings.backend||settings.provider||'claude_cli');setModelId(settings.model);}).catch(e=>setError(message(e)));
  },[providerOpen]);
  useEffect(()=>{
    if(!providerOpen)return;
    let active=true;
    setAgentModels(null);
    invoke<ModelChoice[]>('agent_models',{backend:agentBackend}).then(models=>{if(active)setAgentModels(Array.isArray(models)?models:[]);}).catch(()=>{if(active)setAgentModels([]);});
    return()=>{active=false;};
  },[providerOpen,agentBackend]);
  useEffect(()=>{
    const channel=new Channel<{kind:string;workspace_id:string;turn_id:string;delta?:string;turn?:AgentTurn}>();
    channel.onmessage=event=>{
      const workspace=activeWorkspace.current;if(!workspace)return;
      if(event.kind==='refresh'){refreshChat(workspace).catch(e=>setError(message(e)));return;}
      if(event.workspace_id!==workspace)return;
      if(event.turn?.chat_id&&event.kind==='turn_started')notePendingChat(event.turn.chat_id,event.turn.id||event.turn_id);
      if(event.turn?.chat_id&&event.kind==='turn_finished')noteSettledChat(event.turn.chat_id,event.turn.id||event.turn_id);
      if(event.turn?.chat_id&&selectedChat.current?.id&&event.turn.chat_id!==selectedChat.current.id){void refreshChat(workspace).catch(e=>setError(message(e)));return;}
      if(event.kind==='text_delta'||event.kind==='thinking_delta'){
        // Deltas extend the last step of their kind; a new step always arrives as a full turn.
        const current=liveTurn.current,delta=event.delta||'';
        if(current?.id===event.turn_id&&['running','cancelling'].includes(current.state)){
          const steps=[...(current.steps||[])],last=steps[steps.length-1];
          if(event.kind==='text_delta'&&last?.kind==='text')steps[steps.length-1]={...last,text:last.text+delta};
          if(event.kind==='thinking_delta'&&last?.kind==='thinking')steps[steps.length-1]={...last,text:last.text+delta};
          showTurn({...current,steps,text:event.kind==='text_delta'?current.text+delta:current.text});
        }
      }else if(event.turn){if(liveTurn.current&&liveTurn.current.id!==event.turn.id&&event.kind!=='turn_started')return;showTurn(event.turn);}
      if(event.kind==='turn_started'||event.kind==='turn_finished')refreshChat(workspace).catch(e=>setError(message(e)));
      if(event.kind==='turn_finished')void invoke<Project>('project',{workspace}).then(next=>{if(activeWorkspace.current===workspace)adopt(next);}).catch(error=>setError(message(error)));
      if(event.kind==='turn_finished'&&event.turn?.error?.code==='LLM_NOT_CONFIGURED')setProviderOpen(true);
    };
    return connectNativeStream('connect_agent',channel,e=>setError(message(e)));
  },[]);
  useEffect(()=>{
    const turn=agentTurn,workspace=project?.workspace_id;
    if(!turn||turn.workspace_id!==workspace||!['running','cancelling'].includes(turn.state))return;
    let active=true,pending=false;
    const check=async()=>{
      if(pending)return;pending=true;
      try{
        const latest=await invoke<AgentTurn>('agent_turn',{turnId:turn.id});
        if(!active||activeWorkspace.current!==workspace||liveTurn.current?.id!==turn.id||latest.id!==turn.id||latest.workspace_id!==workspace)return;
        const committed=await invoke<Project>('project',{workspace});
        if(!active||activeWorkspace.current!==workspace)return;
        if(currentProject.current?.revision!==committed.revision||currentProject.current?.dirty!==committed.dirty)adopt(committed);
        // Streaming events own live text. Polling reconciles model and terminal state.
        if(['succeeded','failed','cancelled'].includes(latest.state)){
          if(latest.chat_id)noteSettledChat(latest.chat_id,latest.id);
          showTurn(latest);await refreshChat(workspace);
        }
      }catch{/* Retry transient status failures without interrupting streamed text. */}
      finally{pending=false;}
    };
    const timer=setInterval(()=>void check(),2000);
    return()=>{active=false;clearInterval(timer);};
  },[project?.workspace_id,agentTurn?.id,agentTurn?.state]);
  useEffect(() => { if(hostRef.current?.initialProject){adopt(hostRef.current.initialProject);return;}let active=true;const request=hostRef.current?invoke<Project>('project',{workspace:hostRef.current.workspaceId}):invoke<Project>('initialize');request.then(next=>{if(active)adopt(next);}).catch(e=>{if(active)setError(message(e));});return()=>{active=false;}; },[]);
  useEffect(()=>()=>{
    if(targetFrame.current!==null)cancelAnimationFrame(targetFrame.current);
    if(draftUiFrame.current!==null)cancelAnimationFrame(draftUiFrame.current);
    if(motionUiFrame.current!==null)cancelAnimationFrame(motionUiFrame.current);
    if(interactiveThrottle.current!==null)window.clearTimeout(interactiveThrottle.current);
    for(const timer of settleTimers.current.values())window.clearTimeout(timer);
    pendingTargets.current.clear();
  },[]);
  useEffect(() => {
    if (!container.current) return;
    let active=true;
    setStartupError('');
    try{
      const viewport = new CadViewport(container.current,(entity,origin,batch)=>{if(!active)return;const current=currentProject.current;if(entity&&(!current||entity.revision!==current.revision||!viewport.hasScene(current.workspace_id,current.revision)))return;
        if(origin==='additive'){if(entity)toggleReference(entity);return;}
        if(origin==='mode'){const next=referencesNow.current.filter(item=>!sameEntity(item,selectionIntent.current?.entity));const primary=next[next.length-1]??null;setReferences(next);choosePrimary(primary);viewport.selectEntity(primary);return;}
        if(origin==='marquee'||origin==='marquee-add'){
          const hits=(batch??[]).filter(item=>current&&item.revision===current.revision);
          let next=origin==='marquee-add'?[...referencesNow.current.filter(item=>current&&item.revision===current.revision)]:[] as Entity[];
          for(const hit of hits){if(!next.some(item=>sameEntity(item,hit)))next.push(hit);}
          if(next.length>MAX_REFERENCES){setSelectionNotice(`Select up to ${MAX_REFERENCES} references per message.`);next=next.slice(0,MAX_REFERENCES);}
          const primary=next[next.length-1]??null;
          setReferences(next);choosePrimary(primary);viewport.selectEntity(primary);if(primary&&!toolPanelOpen.current)setModelOpen(true);return;
        }
        if(origin==='replacement'&&current&&intendedSelection(current))return;setSelection(entity,origin!=='replacement');},item=>{if(!active||!item.parameter)return;const current=currentProject.current;if(!current||!viewport.hasScene(current.workspace_id,current.revision))return;setPanel('model');requestAnimationFrame(()=>document.getElementById(`parameter:${item.parameter}`)?.focus());},state=>{if(!active)return;setGraphicsLost(state==='lost');if(state==='restored')setViewAttempt(attempt=>attempt+1);});
      viewer.current=viewport;if(hoverStatus.current)viewport.setStatusContainer(hoverStatus.current);setGraphicsLost(false);
      return()=>{active=false;viewer.current=null;viewport.dispose();};
    }catch(error){viewer.current=null;setStartupError(message(error));return()=>{active=false;};}
  },[viewportAttempt]);
  useEffect(()=>{const retained=project&&viewer.current?.sceneState(project.workspace_id,project.revision);displayRetry.current=false;setHidden(new Set(retained?.hidden||[]));setSelection(intendedSelection(project)||retained?.selection||null);if(cameraIntent.current&&(cameraIntent.current.workspace!==project?.workspace_id||cameraIntent.current.revision!==project?.revision))cameraIntent.current=null;},[project?.workspace_id,project?.revision]);
  useEffect(() => {
    if(host&&!host.visible){viewer.current?.suspend();return;}
    if (!project || !viewer.current || graphicsLost) return;
    let active=true;const viewport=viewer.current;
    const scoped=()=>active&&currentProject.current?.workspace_id===project.workspace_id&&currentProject.current?.revision===project.revision&&currentProject.current?.display_generation===project.display_generation;
    const status=(phase:ViewStatus['phase'],message?:string)=>{if(scoped())setViewStatus({workspace:project.workspace_id,revision:project.revision,generation:project.display_generation,phase,message});};
    if(project.display_error&&viewport.sceneState(project.workspace_id,project.revision)){status('missing',project.display_error.message);return;}
    status('loading');
    const browseGenerated=!host?.modelEntry&&!!project.model_entry;
    viewport.load(project.display_error||browseGenerated?{parts:[]}:project.model,project.revision,handle => invoke<ArrayBuffer>('artifact',{
      workspace:project.workspace_id,revision:project.revision,handle
    }),project.workspace_id).then(()=>{
      if(!scoped())return;
      displayRetry.current=false;
      for(const part of project.model.parts)viewport.setVisible(part.instance_id,!hiddenIntent.current.has(part.instance_id));
      const selected=intendedSelection(project);viewport.selectEntity(selected);setSelection(selected);
      if(currentMotion.current)viewport.setMotion(currentMotion.current);
      const camera=cameraIntent.current;
      if(camera&&camera.workspace===project.workspace_id&&camera.revision===project.revision&&viewport.hasScene(project.workspace_id,project.revision)){cameraIntent.current=null;chooseCamera(camera.action);}
      status(project.display_error?'missing':'ready',project.display_error?.message);
    }).catch(async error => {
      if(!scoped())return;
      if(!displayRetry.current){
        displayRetry.current=true;
        try{
          const fresh=await invoke<Project>('project',{workspace:project.workspace_id});
          if(!scoped())return;
          if(fresh.workspace_id===project.workspace_id&&fresh.revision===project.revision&&(fresh.display_error||fresh.display_generation&&fresh.display_generation!==project.display_generation)){
            adopt(fresh);return;
          }
        }catch{/* Preserve the original failure when metadata cannot recover it. */}
      }
      status('failed',message(error));
    });
    return () => { active=false;viewport.cancelLoad(); };
  },[project?.workspace_id,project?.revision,project?.display_generation,project?.display_error?.code,viewAttempt,graphicsLost,viewportAttempt,host?.visible]);
  useEffect(() => { viewer.current?.setDimensions(dimensions,scope); },[dimensions,scope,viewportAttempt]);
  useEffect(()=>{viewer.current?.setSelectionMode(selectionMode);},[selectionMode,viewportAttempt]);
  useEffect(()=>{viewer.current?.setAdvancedTools(advancedView);},[advancedView,viewportAttempt]);
  useEffect(()=>{setInterferenceResult(null);},[motion?.revision,motion?.pose_version]);
  useEffect(()=>{viewer.current?.setManipulation(manipulating&&motion?.mode==='manipulate'&&!motion.playback,(joint,coordinate,unit,commit)=>{const current=currentProject.current;if(current&&viewer.current?.hasScene(current.workspace_id,current.revision))queueJoint(joint,coordinate,unit,commit);});},[manipulating,motion?.mode,motion?.playback?.recording_id,project?.workspace_id,project?.revision,viewportAttempt]);
  useEffect(() => {
    setProperties(null);setInspectStatus('');
    if (!selection || !project) return;
    let active=true;
    const version=motion?.pose_version||0;
    const timer=setTimeout(()=>invoke<Inspection>('inspect',{workspace:project.workspace_id,revision:selection.revision,args:{entities:[selection],pose_version:version}})
      .then(result => { if(active&&currentMotion.current?.pose_version===version){setInspectStatus('');setProperties(result);} }).catch(e=>{
        if(!active)return;
        // Restored selection / cold worker recovery must not look like the user broke an action.
        const code=errorCode(e);
        if(code==='RECOVERY_GEOMETRY_CHANGED'||code==='RECOVERY_VERIFICATION_REQUIRED'){
          setInspectStatus(code==='RECOVERY_GEOMETRY_CHANGED'
            ?'Exact inspection needs a new build revision. The committed model on screen is unchanged.'
            :'This revision needs an explicit rebuild before exact inspection.');
          return;
        }
        setError(message(e));
      }),150);
    return () => {active=false;clearTimeout(timer);};
  },[selection,project?.revision,motion?.pose_version]);
  useEffect(()=>{
    const live=references.filter(item=>item.revision===project?.revision&&!hidden.has(item.instance_id));
    if(live.length!==references.length){setReferences(live);return;}
    viewer.current?.setReferences(references,references.filter(item=>omitted.has(entityKey(item))));
    // External MCP agents read the same set, in letter order, through get_selection.
    if(project&&(!host||host.active))invoke('set_selection',{workspace:project.workspace_id,revision:project.revision,entities:references.filter(item=>!omitted.has(entityKey(item)))}).catch(()=>{});
  },[references,omitted,hidden,project?.workspace_id,project?.revision,viewportAttempt,viewStatus?.phase,host?.active]);
  useEffect(()=>{liveJob.current=job;},[job]);
  useEffect(() => {
    if (!job || !project || !['queued','running'].includes(job.state)) return;
    let active=true;
    const timer = setTimeout(() => {
      invoke<Job>('job',{workspace:project.workspace_id,jobId:job.id}).then(async next => {
        const refreshed=next.state==='succeeded' ? await invoke<Project>('project',{workspace:project.workspace_id}) : null;
        if(!active || activeWorkspace.current!==project.workspace_id)return;
        liveJob.current=next;setJob(next);
        if(refreshed)adopt(refreshed);

      }).catch(e=>{if(active)setError(message(e));});
    },200);
    return () => {active=false;clearTimeout(timer);};
  },[job,project?.workspace_id]);
  const agentWorking=!!agentTurn&&['running','cancelling'].includes(agentTurn.state);
  const working = busy || agentSending || agentWorking || !!job && ['queued','running'].includes(job.state);
  const simulationUnavailable=motion?.simulation_unavailable_reason||(motion&&motion.simulation_unavailable_reason===undefined&&(motion.constraints||motion.joints.some(joint=>joint.kind==='path'))?'Simulation for this mechanism is not available yet':null);
  const [importCandidates,setImportCandidates]=useState<string[]>([]);
  const [importChoice,setImportChoice]=useState('');
  const importPrompted=useRef(new Set<string>());
  useEffect(()=>{
    const candidates=project?.import_candidates??[];
    if(!project||!candidates.length||importPrompted.current.has(project.workspace_id))return;
    importPrompted.current.add(project.workspace_id);
    setImportCandidates(candidates);setImportChoice(candidates[0]??'');
  },[project?.workspace_id,project?.import_candidates]);
  async function run(command:string,args:Record<string,unknown>={}) {
    const focus=hostRef.current?.chrome?focusProject:null;
    const target=focus&&hostRef.current?.focusWorkspace===focus.workspace_id?focus:currentProject.current;
    const workspace=target?.workspace_id??project?.workspace_id;
    if(!workspace||mutationBusy.current)return;
    if(liveJob.current&&['queued','running'].includes(liveJob.current.state)){
      setError('Wait for the current build to finish before changing the model.');
      return;
    }
    mutationBusy.current=true;
    setBusy(true);setError('');setNotice('');
    try {
      const revision=target?.revision;
      if(!revision)return;
      const next=await invoke<Job&{cancelled?:boolean}>(command,{workspace,revision,...args});
      if(!next.cancelled){
        liveJob.current=next;setJob(next);
        if(focus&&workspace===focus.workspace_id)invoke<Project>('project',{workspace}).then(setFocusProject).catch(()=>{});
      }
    } catch(e){setError(message(e));}finally{mutationBusy.current=false;setBusy(false);}
  }
  async function openModelEntry(entry:string,builder:string){
    setLibraryOpen(false);
    if(!hostRef.current?.onOpenModel){await run('work_on_project_builder',{entry,builder});return;}
    // Leave the Explorer row clickable. The canvas shows loading immediately, and a later click cancels this open.
    await hostRef.current.onOpenModel(entry,builder);
  }
  async function download(format:string) {
    if(!project)return;
    const workspace=project.workspace_id;
    try {
      const result=await invoke<{cancelled?:boolean;name:string;revision:string}>('export',{workspace,revision:project.revision,format,poseVersion:currentMotion.current?.pose_version});
      if(!result.cancelled && activeWorkspace.current===workspace)setNotice(`${result.name} exported from the committed model.`);
    } catch(e){if(activeWorkspace.current===workspace)setError(message(e));}
  }
  const [libraryOpen,setLibraryOpen]=useState(false);
  const [quickModelOpen,setQuickModelOpen]=useState(false);
  useEffect(()=>setLibraryOpen(false),[project?.workspace_id]);
  useEffect(()=>{if(!project)setQuickModelOpen(false);},[project?.workspace_id]);
  useEffect(()=>{
    if(host&&!host.active)return;
    const openQuickModel=(event:KeyboardEvent)=>{
      if(!(event.metaKey||event.ctrlKey)||!event.shiftKey||event.altKey||event.code!=='KeyO'||!currentProject.current)return;
      event.preventDefault();setQuickModelOpen(true);
    };
    window.addEventListener('keydown',openQuickModel);return()=>window.removeEventListener('keydown',openQuickModel);
  },[host?.active]);
  const [entrySelection,setEntrySelection]=useState<EntrySelection|null>(null);
  const [entryError,setEntryError]=useState('');
  async function openSelectedEntry(entry?:string,adapter?:string){
    if(!entrySelection)return;
    const selection=entrySelection;setBusy(true);setEntryError('');
    try{
      const result=await invoke<{cancelled?:boolean;project:Project;job:Job}>('open_project_entry',{requestId:selection.request_id,entry:entry??null,...(adapter?{adapter}:{})});
      setEntrySelection(null);
      if(result.cancelled)return;
      if(hostRef.current){hostRef.current.onOpen(result.project,{replace:true});return;}
      adopt(result.project);setJob(result.job);setSelection(null);setExternal(null);setNotice('');
    }catch(error){if(entry===undefined)setEntrySelection(null);else setEntryError(message(error));}finally{setBusy(false);}
  }
  async function setupProjectEntry(){
    if(!project)return;setBusy(true);setError('');
    try{const result=await invoke<{choose_entry:EntrySelection}>('configure_project_entry',{workspace:project.workspace_id});setEntryError('');setEntrySelection(result.choose_entry);}
    catch(error){setError(message(error));}finally{setBusy(false);}
  }
  async function open(command:'open_project'|'new_scratchpad',options?:{replace?:boolean}) {
    if(entrySelection)return;
    setBusy(true);setError('');
    try {
      if(command==='new_scratchpad') {const next=await invoke<Project>(command);if(hostRef.current){hostRef.current.onOpen(next,options);return;}adopt(next);setJob(null);}
      else {
        const result=await invoke<{cancelled?:boolean;choose_entry?:EntrySelection;project:Project;job:Job}>(command);
        if(result.cancelled)return;
        if(result.choose_entry){setEntryError('');setEntrySelection(result.choose_entry);return;}
        if(hostRef.current){hostRef.current.onOpen(result.project,{replace:true});setEntrySelection(null);return;}
        adopt(result.project);setJob(result.job);
      }
      setSelection(null);setExternal(null);setNotice('');
    }catch(e){setError(message(e));}finally{setBusy(false);}
  }
  async function newProject(){
    if(busy||entrySelection)return;
    setBusy(true);setError('');
    try{
      const draft=await invoke<Project>('new_scratchpad');
      const created=await invoke<Project&{cancelled?:boolean}>('save_project',{workspace:draft.workspace_id,revision:draft.revision,saveAs:true});
      if(created.cancelled)return;
      if(hostRef.current){hostRef.current.onOpen(created);return;}
      adopt(created);setJob(null);setSelection(null);setExternal(null);setNotice('');
    }catch(error){setError(message(error));}finally{setBusy(false);}
  }
  async function cleanCache(){
    setCleaningCache(true);setError('');
    try{const report=await invoke<{bytes_removed:number;already_running:boolean;retained_unmanaged:number;errors:string[];evicted_display_revisions:number;display_budget_exceeded:boolean}>('clean_cache');setNotice(report.already_running?'Cache cleanup is already running.':`Removed ${(report.bytes_removed/1024/1024).toFixed(1)} MB of cached data.${report.evicted_display_revisions?` Display data for ${report.evicted_display_revisions} workspace(s) can be restored when needed.`:''}${report.display_budget_exceeded?' Active or protected display data remains above the cache target.':''}${report.retained_unmanaged?' Unmanaged files and folders were left untouched.':''}`);if(report.errors.length)setError(report.errors.join('\n'));}
    catch(error){setError(message(error));}finally{setCleaningCache(false);}
  }
  async function save(saveAs=false) {
    if(!project)return;
    const workspace=project.workspace_id;
    setBusy(true);setError('');
    try {
      const result=await invoke<Project&{cancelled?:boolean}>('save_project',{workspace,revision:project.revision,saveAs});
      if(!result.cancelled && activeWorkspace.current===workspace)adopt(result);
    }catch(e){if(activeWorkspace.current===workspace)setError(message(e));}finally{setBusy(false);}
  }
  // Bound projects write through quietly whenever the committed model changes.
  const externalNow=useRef(external);externalNow.current=external;
  const autoSaving=useRef(false),autoSaveTimer=useRef<number|null>(null);
  async function autoSaveBound(){
    const current=currentProject.current;
    if(!current||!hasUnsavedChanges(current)||current.saved_revision==null||autoSaving.current)return;
    if(liveJob.current&&['queued','running'].includes(liveJob.current.state))return;
    autoSaving.current=true;
    const workspace=current.workspace_id;
    try{
      const result=await invoke<Project&{cancelled?:boolean}>('save_project',{workspace,revision:current.revision,saveAs:false});
      if(!result.cancelled&&activeWorkspace.current===workspace)adopt(result);
    }catch(error){if(activeWorkspace.current===workspace)setError(message(error));}
    finally{autoSaving.current=false;}
  }
  useEffect(()=>{
    if(!project||!hasUnsavedChanges(project)||project.saved_revision==null)return;
    if(autoSaveTimer.current!==null)window.clearTimeout(autoSaveTimer.current);
    autoSaveTimer.current=window.setTimeout(()=>void autoSaveBound(),450);
    return()=>{if(autoSaveTimer.current!==null)window.clearTimeout(autoSaveTimer.current);};
  },[project?.dirty,project?.unsaved_changes,project?.model_source_dirty,project?.revision,project?.saved_revision,project?.workspace_id,job?.state,agentTurn?.state,external?.changed]);
  useEffect(() => {
    if(!project || !project.external_tracking){setExternal(null);return;}
    let active=true;
    let pending=false;reviewFingerprint.current='';
    const workspace=project.workspace_id;
    const check=async()=>{
      if(pending)return;
      pending=true;
      try {
        const next=await invoke<{changed:boolean;fingerprint:string;changes:Record<string,string>;conflicts?:string[];source_kind?:string}>('check_external',{workspace});
        if(active && activeWorkspace.current===workspace){if(reviewFingerprint.current!==next.fingerprint){reviewFingerprint.current=next.fingerprint;setExternalResolutions({});}setExternal(next);}
      }catch(e){if(active)setError(message(e));}finally{pending=false;}
    };
    void check();const timer=setInterval(check,2500);
    return()=>{active=false;clearInterval(timer);};
  },[project?.workspace_id,project?.revision,project?.saved_revision,project?.external_tracking]);
  function prepareMotionTest(intent:string,id?:string){
    const request=(id?`Edit the saved CAD requirement with ID ${id}. List saved requirements and inspect its existing definition first.\n`:'Create a saved CAD requirement from this free-text requirement:\n')+intent+'\nInspect the current assembly and save explicit starting conditions, actions and measurable pass/fail checks. Ask for missing targets, tolerances or simulation assumptions; do not invent acceptance criteria. If the requirement cannot be expressed with supported checks, explain what is missing instead of claiming it is verified. Prepare and save the requirement and its simulation checks only: do not run them. The user will review the saved checks in Requirements and click Check requirement. Explain the prepared checks in plain language. CAD simulation only.';
    setPanel('assistant');setPrompt(request);void sendPrompt(request);
  }
  function intendedSelection(current:Project|null):Entity|null {
    const intent=selectionIntent.current;
    return current&&intent?.workspace===current.workspace_id&&!hiddenIntent.current.has(intent.entity.instance_id)?sceneEntity(current.model,current.revision,intent.entity):null;
  }
  function chooseCamera(action:CameraAction){
    const current=currentProject.current;if(!current)return;
    if(viewer.current?.hasScene(current.workspace_id,current.revision)){
      cameraIntent.current=null;if(action==='fit')viewer.current.resetView();else viewer.current.view(action);
    }else cameraIntent.current={workspace:current.workspace_id,revision:current.revision,action};
  }
  function nativePopup(kind:'export'|'selection'|'scope',element:HTMLElement){
    const rect=element.getBoundingClientRect();void invoke('popup_native_menu',{kind,x:rect.left,y:rect.bottom+3}).catch(e=>setError(message(e)));
  }
  function showAllParts(){
    applyVisibility(new Set());
  }
  function applyVisibility(next:Set<string>){
    if(!project)return;setHidden(next);if(selectionIntent.current&&next.has(selectionIntent.current.entity.instance_id))setSelection(null);
    if(viewer.current?.hasScene(project.workspace_id,project.revision))for(const part of project.model.parts)viewer.current.setVisible(part.instance_id,!next.has(part.instance_id));
  }
  function isolatePart(){
    if(!project||!selection)return;
    const next=new Set(project.model.parts.filter(part=>part.instance_id!==selection.instance_id).map(part=>part.instance_id));
    applyVisibility(next);
  }
  menuAction.current=action=>{
    if(action.startsWith('workspace:')){const id=action.slice('workspace:'.length);if(!busy&&workspaces.some(ws=>ws.workspace_id===id))void switchWorkspace(id);return;}
    switch(action){
      case 'settings':setProviderOpen(true);break;
      case 'new':
      case 'new-model':if(!busy)void open('new_scratchpad');break;
      case 'new-project':void newProject();break;
      case 'open':if(!busy)void open('open_project');break;
      case 'save':if(!busy)void save();break;
      case 'save-as':if(!busy)void save(true);break;
      case 'import':if(!working)void run('import_model');break;
      case 'rebuild':if(!working)void run('rebuild');break;
      case 'export-step':void download('STEP');break;
      case 'export-stl':void download('STL');break;
      case 'undo':if(document.activeElement?.matches('input:not([type=range]):not([type=checkbox]),textarea,[contenteditable=true]')){document.execCommand?.('undo');}else if(!working&&project?.activity.some(job=>job.state==='succeeded'))void run('undo');break;
      case 'select-face':setSelectionMode('face');break;case 'select-edge':setSelectionMode('edge');break;case 'select-part':setSelectionMode('part');break;
      case 'scope-selected':setScope('selected');break;case 'scope-pinned':setScope('pinned');break;case 'scope-all':setScope('all');break;
      case 'dimensions':setDimensions(value=>!value);break;
      case 'advanced':setAdvancedView(value=>!value);break;
      case 'isolate':isolatePart();break;
      case 'show-all':showAllParts();break;
      case 'fit':chooseCamera('fit');break;
      case 'iso':case 'front':case 'rear':case 'top':case 'bottom':case 'left':case 'right':chooseCamera(action);break;
      case 'panel-model':if(host){host.showTool('model',true);break;}if(modelOpen)setModelOpen(false);else setPanel('model');break;case 'panel-motion':openMotionTool(motionTask,true);break;case 'panel-assistant':setPanel('assistant');break;
    }
  };
  useEffect(()=>{
    let active=true;const channel=messageChannel<string>(!!hostRef.current);channel.onmessage=action=>{if(active)menuAction.current(action);};
    void (hostRef.current?hostRef.current.connectMenu(channel):invoke('connect_native_menu',{channel})).catch(e=>{if(active)setError(message(e));});return()=>{active=false;channel.onmessage=()=>{};};
  },[]);
  useEffect(()=>{
    const refresh=()=>setEditing(!!document.activeElement?.matches('input:not([type=range]):not([type=checkbox]),textarea,[contenteditable=true]'));
    document.addEventListener('focusin',refresh);document.addEventListener('focusout',refresh);return()=>{document.removeEventListener('focusin',refresh);document.removeEventListener('focusout',refresh);};
  },[]);
  useEffect(()=>{if(host&&!host.active)return;void invoke('update_native_menu',{status:{has_project:!!project,has_parts:!!project?.model.parts.length,working,busy,can_undo:!!project?.activity.some(job=>job.state==='succeeded'),editing,dimensions,advanced:advancedView,document_name:project?.name||'Untitled',inspector:panel,model_browser_open:modelOpen,motion_panel_open:motionOpen,has_selection:!!selection,has_hidden_parts:!!hidden.size,selection_mode:selectionMode,dimension_scope:scope,workspaces:workspaces.filter(ws=>!ws.model_parent).map(ws=>({id:ws.workspace_id,name:ws.name,selected:ws.workspace_id===(project?.model_parent||project?.workspace_id),running:!!ws.running}))}}).catch(e=>setError(message(e)));},[project?.workspace_id,project?.name,project?.revision,project?.model.parts.length,working,busy,editing,dimensions,advancedView,panel,modelOpen,motionOpen,selection,hidden,selectionMode,scope,workspaces,host?.active]);
  useEffect(()=>{
    const dialog=settingsDialog.current;if(!dialog)return;
    if(providerOpen&&!dialog.open)dialog.showModal();else if(!providerOpen&&dialog.open)dialog.close();
  },[providerOpen]);
  const scenePhase=startupError?'failed':graphicsLost?'lost':!project?'idle':viewStatus?.workspace===project.workspace_id&&viewStatus.revision===project.revision&&viewStatus.generation===project.display_generation?viewStatus.phase:'loading';
  const agentChromeStatus=agentStartCancelling?'Stopping…':agentWorking?agentTurn.activity:agentSending?'Starting request…':'';
  const projectBound=!!project&&(project.saved_revision!=null||project.external_tracking);
  const retainedView=!!project&&!!viewer.current?.hasScene(project.workspace_id,project.revision);
  const generatedProjectView=!host?.modelEntry&&!!project?.model_entry;
  const modelInCanvas=!!project?.model.parts.length&&!generatedProjectView&&!project.display_error&&!libraryOpen&&(scenePhase==='ready'||retainedView);
  const [modelHistory,setModelHistory]=useState<{label?:string;revision:string;restored:boolean;initial:boolean;actor:string;parts:number;created_at?:number}[]>([]);
  useEffect(()=>{
    if(!project)return;
    let active=true;
    invoke<typeof modelHistory>('model_history',{workspace:project.workspace_id}).then(items=>{if(active)setModelHistory(items);}).catch(()=>{});
    return()=>{active=false;};
  },[project?.workspace_id,project?.revision]);
  const headerBar=(    <header className="app-header" data-tauri-drag-region>
      <div className="document-title" data-tauri-drag-region title={projectBound?`Open project folder · ${project?.name||'Untitled'}`:`Model · ${project?.name||'Untitled'}`}>
        <Icon name={projectBound?'folder':'cube'} size={16}/>
        <span className="document-identity"><span className="document-name">{project?.name||'Untitled'}</span></span>
        {project&&hasUnsavedChanges(project)&&<span className="document-edited">Edited</span>}
      </div>
      <span className="header-spacer" data-tauri-drag-region/>
      <ModelHistory entries={modelHistory} disabled={working} onUndo={()=>void run('undo')} onRestore={target=>void run('restore_checkpoint',{target})}/>
      {external?.changed&&<button type="button" className="external-changes-trigger" aria-haspopup="dialog" aria-label="Review files changed outside Cadpad" onClick={()=>setExternalReviewOpen(true)}><Icon name="file" size={14}/>Files changed <span>{Object.keys(external.changes).length}</span></button>}


      <button id="tab-assistant" className="titlebar-assistant"  aria-label="Toggle Assistant panel" title="Toggle Assistant" aria-controls="panel-assistant-dock" aria-expanded={host?host.visibleTools.includes('assistant'):assistantOpen} onClick={()=>{if(host)host.showTool('assistant',true);else setAssistantOpen(!assistantOpen);setFocusedTool('assistant');}}><Icon name="chat" size={16}/><span>Assistant</span></button>
    </header>);
  const activityRail=(      <nav className="activity-rail" aria-label="Workspace panels">
        <button id="tab-model" {...railEvents('model','Explorer')} aria-label="Toggle Explorer panel" data-sidebar-tooltip="Explorer" aria-controls="panel-model" aria-expanded={host?host.visibleTools.includes('model'):modelOpen} onClick={()=>{if(host)host.showTool('model',true);else if(modelOpen)setModelOpen(false);else setPanel('model');}}><Icon name="folder" size={21}/></button>
        {motionTools.filter(tool=>tool.id!=='simulate').map(tool=><button key={tool.id} id={tool.button} {...railEvents(tool.id,tool.title)} aria-label={`Toggle ${tool.title} panel`} data-sidebar-tooltip={tool.title} aria-controls={host?`panel-${tool.id}`:"panel-motion"} aria-expanded={host?host.visibleTools.includes(tool.id):motionOpen&&motionTask===tool.id} onClick={()=>openMotionTool(tool.id,true)}><Icon name={tool.icon} size={21}/></button>)}

      </nav>);
  const noticesPanel=(        <div className="workspace-notices inspector-page">      {notice&&<p className="notice">{notice}</p>}
      {project?.display_error&&<section><p>{project.display_error.message}</p><button disabled={working} onClick={()=>run('repair_display')}>Restore display</button><button disabled={working} onClick={()=>run('rebuild')}>Rebuild as new revision</button></section>}



</div>);
  const explorer=displayProject||project;
  const modelPanel=(          <aside className="model-browser explorer-panel inspector-page" id="panel-model" aria-label="Explorer" hidden={!host&&!modelOpen}>
            {!host&&<PanelResizeHandle panel="model" value={panelSizes.model} onChange={value=>resizePanel('model',value)}/>}
            {!host&&<header className="dock-heading"><strong>Explorer</strong><button className="icon-button" aria-label="Close Explorer" onClick={()=>{setModelOpen(false);requestAnimationFrame(()=>document.getElementById('tab-model')?.focus());}}><Icon name="close" size={16}/></button></header>}
            <div className="model-browser-content">
          {explorer?<ProjectExplorer catalogWorkspace={host?.chrome?project?.workspace_id:undefined} catalogRevision={host?.chrome?project?.revision:undefined} activeModel={host?.chrome?host.focusModelEntry:host?.modelEntry} openingModel={host?.openingModel} selectionPanel={selection?<div><p className="selection-name">{explorer.model.parts.find(part=>part.instance_id===selection.instance_id)?.name||selection.instance_id}<span>{selection.entity_id.startsWith('part:')?'Object':humanLabel(selection.entity_id.split(':')[0])}</span></p><button onClick={isolatePart}>Isolate object</button><p className="muted">Measured on the current model.</p>{inspectStatus?<><p className="muted">{inspectStatus}</p><button disabled={working} onClick={()=>run('rebuild')}>Rebuild as new revision</button></>:<PropertySummary inspection={properties}/>}</div>:undefined} projectName={explorer.name||undefined} onBrowseLibrary={()=>setLibraryOpen(true)} workspace={explorer.workspace_id} revision={explorer.revision} builderDisabled={host?busy||agentSending||agentWorking:working} onChooseBuilder={(entry,builder)=>void openModelEntry(entry,builder)} parts={generatedProjectView&&explorer.workspace_id===project?.workspace_id&&!host?.focusModelEntry?[]:explorer.model.parts} parameters={Object.entries(explorer.parameters).map(([name,p])=><label className="parameter" key={`${explorer.revision}:${name}`}><span title={p.description}>{humanLabel(name)}</span><input id={`parameter:${name}`} type="number" defaultValue={p.value} min={p.minimum??undefined} max={p.maximum??undefined} disabled={working} onBlur={e=>{const value=Number(e.target.value);if(value!==p.value)run('parameter',{parameter:name,value,unit:p.unit});}} onKeyDown={e=>{if(e.key==='Enter')e.currentTarget.blur();}}/><small>{p.unit}</small></label>)} assets={explorer.assets||{}} selected={selection?.instance_id} hidden={hidden} onShowAll={showAllParts} onSelect={id=>{const entity={revision:explorer.revision,instance_id:id,entity_id:'part:0'};if(viewer.current?.hasScene(explorer.workspace_id,explorer.revision))viewer.current.select(id);else window.dispatchEvent(new CustomEvent('cadpad-canvas-select',{detail:{workspace:explorer.workspace_id,revision:explorer.revision,instanceId:id}}));setSelection(entity);}} onToggleVisibility={id=>{const next=new Set(hiddenIntent.current);if(next.has(id))next.delete(id);else next.add(id);applyVisibility(next);window.dispatchEvent(new CustomEvent('cadpad-canvas-visibility',{detail:{workspace:explorer.workspace_id,hidden:[...next]}}));}}/>:<div className="panel-empty"><h3>No project open</h3><p>Open a project to browse its source and geometry.</p><button disabled={working} onClick={()=>open('open_project')}>Open a project</button></div>}

        </div>
          </aside>);
  const motionPanel=(task:MotionTask)=>(            <aside className="motion-sidebar inspector-page" id={host?`panel-${task}`:"panel-motion"} aria-label={motionTools.find(tool=>tool.id===task)!.title} hidden={!host&&!motionOpen}>
              {!host&&<PanelResizeHandle panel="motion" value={panelSizes.motion} onChange={value=>resizePanel('motion',value)}/>}
              {!host&&<header className="dock-heading"><strong>{motionTools.find(tool=>tool.id===task)!.title}</strong>{task!=='test'&&<span className="motion-state" role="status">{motion?.playback?'Replay · live motion paused':motion?.mode==='simulate'?(simulationRunning?'Physics running':'Physics paused'):manipulating?'Drag mode':'Gravity off'}{motion?.recording?' · recording':''}</span>}<button className="icon-button" aria-label="Close motion panel" onClick={()=>{setMotionOpen(false);requestAnimationFrame(()=>document.getElementById(motionTools.find(tool=>tool.id===task)!.button)?.focus());}}><Icon name="close" size={16}/></button></header>}
              <div className="motion-sidebar-content">

          {motion?.playback&&<div className="motion-session" role="status"><strong>Watching a replay</strong><p>The canvas shows a saved moment. Return to live movement before moving parts or starting a simulation.</p><button disabled={playbackBusy} onClick={()=>{setPlaybackRunning(false);void playbackAction({action:'leave'});}}>Leave playback</button></div>}
          {motion?.mode==='simulate'&&task==='move'&&!motion.playback&&<div className="motion-session"><strong>Physics is paused</strong><p>Return to the position from before the simulation to move objects by hand, or open Physics to continue.</p><button disabled={simulationBusy||!!motion.recording} onClick={()=>{setSimulationRunning(false);void simulationAction({action:'stop'});}}>Stop and restore pose</button>{motion.recording&&<p>Finish or discard your recording first.</p>}</div>}
          {(!host||task!=='test')&&<div hidden={task==='test'}>
          {motion?.recording&&!motion.playback&&<div className="motion-capture-bar"><span>Recording · {motion.recording.duration_s.toFixed(2)} s{motion.recording.capacity_reached?' · full':''}</span><button disabled={playbackBusy} onClick={()=>void playbackAction({action:'finish_recording'})}>Finish recording</button></div>}
          {task==='move'&&project&&<JointWizard key={`${project.workspace_id}:${project.revision}`} parts={project.model.parts} motion={motion} disabled={working||motion?.mode==='simulate'||!!motion?.playback||!!motion?.recording} onSave={async joint=>{await run('add_joint',{joint});}}/>}
        {task==='simulate'&&project&&motion&&!motion.collision&&<div className="panel-empty"><h3>Physics needs contact geometry</h3><p>{simulationUnavailable||'Ask the assistant to configure collision shapes for this assembly before simulating forces.'}</p><button onClick={()=>{setPrompt('Set up this assembly for physics simulation, including collision shapes and which objects stay fixed.');setPanel('assistant');}}>Set up with assistant</button></div>}
        {project&&motion?.collision&&(!host||task==='simulate')&&<div className="physics-workflow" hidden={task!=='simulate'}><h3>{motion.mode==='simulate'?'Run and observe':'Choose what can move'}</h3><section><p className="muted">Objects respond to gravity, motor forces and contact. This is an approximate physics preview.</p>{motion.mode==='simulate'&&!motion.playback?<><p className="muted">{motion.simulation?.current.time_s.toFixed(3)} s · fixed step 1/120 s · approximate proxy inertia and contacts</p><button disabled={simulationBusy} onClick={()=>setSimulationRunning(!simulationRunning)} className="primary-button">{simulationRunning?'Pause simulation':'Run simulation'}</button><button disabled={simulationBusy||simulationRunning} onClick={()=>void simulationAction({action:'advance',steps:2})}>Advance 1/60 s</button><button disabled={simulationBusy||!!motion.recording} onClick={()=>{setSimulationRunning(false);void simulationAction({action:'stop'});}}>Stop and restore pose</button></>:<><p className="muted">Keep the base in place and let moving objects respond to forces. Review the choices below, then start.</p>{project.model.parts.map(part=><label className="parameter" key={part.instance_id}><span>{part.name}</span><select value={(simulationDynamic[part.instance_id]??motion.joints.some(joint=>joint.child===part.instance_id))?'dynamic':'fixed'} onChange={event=>setSimulationDynamic({...simulationDynamic,[part.instance_id]:event.target.value==='dynamic'})}><option value="fixed">Stay in place</option><option value="dynamic">Can move</option></select></label>)}<PhysicsSettings density={simulationDensity} friction={simulationFriction} bounce={simulationRestitution} gravity={simulationGravity} onDensity={setSimulationDensity} onFriction={setSimulationFriction} onBounce={setSimulationRestitution} onGravity={setSimulationGravity}/><button className="primary-button" disabled={simulationBusy||!!motion.recording||!!motion.playback||!!simulationUnavailable} onClick={()=>void startSimulation()}>Run simulation</button>{simulationUnavailable&&<p className="muted">{simulationUnavailable}</p>}</>}<details><summary>Advanced motor settings</summary><p className="muted">Sliders: N, N/m, N s/m. Hinges: N m, N m/rad, N m s/rad.</p><label>Maximum force / torque<input type="number" min={0.000001} value={motorForce} onChange={event=>setMotorForce(Number(event.target.value))}/></label><label>Spring stiffness<input type="number" min={0} value={motorStiffness} onChange={event=>setMotorStiffness(Number(event.target.value))}/></label><label>Damping<input type="number" min={0} value={motorDamping} onChange={event=>setMotorDamping(Number(event.target.value))}/></label></details></section>{motion?.mode==='simulate'&&motion.simulation&&<section><ExternalLoads key={`${motion.workspace_id}:${motion.revision}`} simulation={motion.simulation} disabled={simulationBusy||simulationRunning||!!motion.playback} onApply={loads=>void simulationAction({action:'loads',loads},motion)}/></section>}</div>}
      {!!motion?.joints.some(joint=>joint.kind!=='fixed'&&joint.drivable!==false)&&(!host||task==='move'&&motion.mode!=='simulate'||task==='simulate'&&motion.mode==='simulate')&&<section className="manual-motion" hidden={task==='move'?motion.mode==='simulate':motion.mode!=='simulate'}>{motion.mode==='simulate'?<><h3>Motor targets</h3><div className="joint-controls">{motion.joints.filter(joint=>joint.kind!=='fixed'&&joint.drivable!==false).map(joint=><JointControl key={`${motion.revision}:${joint.id}`} joint={joint} name={humanLabel(project?.model.parts.find(part=>part.instance_id===joint.child)?.name||joint.id)} coordinate={draftCoordinates.current.get(joint.id)??joint.coordinate} disabled={!!motion.playback} motor={motion.mode==='simulate'} onPreview={next=>{viewer.current?.previewCoordinate(joint.id,next);queueJoint(joint.id,next,joint.unit,false);}} onCommit={next=>queueJoint(joint.id,next,joint.unit,true)}/>)}</div></>:project&&<MotionParts parts={project.model.parts} motion={motion} dragging={manipulating} disabled={!!motion.playback} coordinates={draftCoordinates.current} onDragging={setManipulating} onLocate={id=>{const entity={revision:project.revision,instance_id:id,entity_id:'part:0'};viewer.current?.selectEntity(entity);setSelection(entity);}} onPreview={(joint,next)=>{viewer.current?.previewCoordinate(joint.id,next);queueJoint(joint.id,next,joint.unit,false);}} onCommit={(joint,next)=>queueJoint(joint.id,next,joint.unit,true)}/>}<details className="motion-technical"><summary>Movement limits and collisions</summary>{motion.constraints&&<p className="muted">{motion.mode==='simulate'?`Approximate loop closure · ${(motion.constraints.position_error_m*1000).toPrecision(3)} mm · ${motion.constraints.axis_error_rad.toPrecision(3)} rad`:'Coupled mechanism · model constraints satisfied'}</p>}<p className="muted">{motion.mode==='simulate'?'Approximate dynamic contacts · force-limited motors':motion.collision?`Swept collision blocking · conservative proxies · ${(motion.collision.clearance_m*1000).toPrecision(3)} mm margin`:'Joint limits apply. Collision blocking is not configured.'}</p>{!!motion.collision?.ignored_pairs.length&&<details><summary>{motion.collision.ignored_pairs.length} intentional contact pair(s) excluded</summary>{motion.collision.ignored_pairs.map(pair=><p key={pair.join(':')}>{pair.join(' ↔ ')}</p>)}</details>}</details></section>}


      </div>}
      <div hidden={task!=='move'&&task!=='simulate'}>
      {task==='move'&&(project?.model.parts.length??0)>1&&<details className="motion-disclosure"><summary>Check for overlapping parts<small>Inspect the current position</small></summary><section className="motion-interference"><h3>Check for overlapping parts</h3><p className="muted">Check the exact geometry at the current position. Move the joints to check another position.</p><button disabled={checkingInterference} onClick={()=>void checkInterference()}>{checkingInterference?'Checking exact geometry…':'Check interference'}</button>{interferenceResult?.exact?.pairs.map(pair=><p key={pair.instances.join(':')}>{pair.instances.join(' ↔ ')}: {pair.interfering?`overlap ${pair.intersection_volume_mm3.toPrecision(4)} mm³`:`clearance ${pair.distance_mm.toPrecision(4)} mm`}<small className="muted"> · exact CAD at the settled pose</small></p>)}</section></details>}
          </div>
          <div hidden={task!=='test'}>{project&&(!host||task==='test')&&<Requirements workspace={project.workspace_id} revision={project.revision} motion={motion} onSnapshot={snapshot=>{setSimulationRunning(false);setPlaybackRunning(false);pendingTargets.current.clear();receiveMotion(snapshot);}} onChanged={()=>{const workspace=project.workspace_id;void invoke<Project>('project',{workspace}).then(next=>{if(activeWorkspace.current===workspace&&currentProject.current?.revision===next.revision){currentProject.current=next;setProject(next);}}).catch(e=>setError(message(e)));}} agentBusy={agentWorking||agentSending} onCreate={intent=>prepareMotionTest(intent)} onEdit={(id,intent)=>prepareMotionTest(intent,id)}/>}</div>
        </div>
            </aside>);
  const canvasPanel=(            <section className={`workspace ${modelInCanvas?'has-model':''}`} aria-label="3D modeling canvas">
      <div className="workspace-stage">
      {modelInCanvas&&<nav className="viewport-tools" aria-label="Model tools">
        <div className="tool-group"><Icon name="cube" size={16}/><button className="native-picker" aria-label="Selection kind" title="Click picks one item. Shift-drag selects by rectangle. ⌘/Ctrl-click adds or removes." aria-haspopup="menu" onClick={event=>nativePopup('selection',event.currentTarget)}>Pick {({face:'faces',edge:'edges',part:'objects'})[selectionMode]}<Icon name="chevron" size={10}/></button></div><span className="tool-divider"/>
        <button className={dimensions?'active':''} aria-pressed={dimensions} onClick={()=>setDimensions(!dimensions)}><Icon name="measure" size={16}/>Dimensions</button>

        {dimensions&&<button className="native-picker" aria-label="Dimension scope" aria-haspopup="menu" onClick={event=>nativePopup('scope',event.currentTarget)}>{({selected:'Selected object',pinned:'Pinned',all:'All visible'})[scope]}<Icon name="chevron" size={10}/></button>}
      </nav>}
      <div className="viewport" ref={container} aria-busy={scenePhase==='loading'||scenePhase==='lost'}/>
      {(scenePhase==='lost'||scenePhase==='loading'||scenePhase==='failed'||scenePhase==='missing'&&!retainedView)&&<div className={`view-recovery ${retainedView?'retained-view':''}`}><div className="view-recovery-card" role={scenePhase==='failed'?'alert':'status'}>
        <strong>{startupError?'Unable to start the 3D view':scenePhase==='lost'?'Waiting for graphics…':scenePhase==='loading'?'Loading model…':scenePhase==='missing'?'Display data needs restoring':'Unable to display this model'}</strong>
        {scenePhase==='lost'?<p>The graphics connection was interrupted. Your model is still available to save or export. The view will reload when graphics return.</p>:scenePhase!=='loading'&&<p>{startupError||viewStatus?.message||project?.display_error?.message}</p>}
        {scenePhase==='failed'&&<button onClick={()=>{displayRetry.current=false;if(!viewer.current)setViewportAttempt(attempt=>attempt+1);else setViewAttempt(attempt=>attempt+1);}}>Retry view</button>}
        {scenePhase==='missing'&&<button disabled={working} onClick={()=>void run('repair_display')}>Restore display</button>}
      </div></div>}

      {!!importCandidates.length&&<div className="import-setup-backdrop"><section className="import-setup" role="dialog" aria-modal="true" aria-labelledby="import-setup-title"><header><h2 id="import-setup-title">Import model</h2><button className="icon-button" aria-label="Close import setup" onClick={()=>setImportCandidates([])}><Icon name="close" size={15}/></button></header><div className="import-setup-body"><p>Choose a model from this project folder.</p><div className="import-file-list" role="radiogroup" aria-label="Model file">{importCandidates.map(path=><button key={path} role="radio" aria-checked={importChoice===path} className={importChoice===path?'selected':''} onClick={()=>setImportChoice(path)}><Icon name="file" size={18}/><span>{path.split('/').at(-1)}</span><small>{path.split('.').at(-1)?.toUpperCase()}</small></button>)}</div><p className="import-setup-hint">The original file stays in your folder.</p></div><footer><button disabled={working} onClick={()=>setImportCandidates([])}>Skip for now</button><button className="primary-button" disabled={working||!importChoice} onClick={()=>{const path=importChoice;setImportCandidates([]);void run('import_model',{selectedPath:path});}}><Icon name="import" size={14}/>Import</button></footer></section></div>}

      {host?.modelEntry&&!project?.model.parts.length&&job&&['queued','running'].includes(job.state)&&<div className="empty model-opening" role="status"><span className="model-opening-spinner" aria-hidden="true"/><h1>Opening model…</h1><p>Building geometry for this tab.</p></div>}
      {(!host?.modelEntry||libraryOpen)&&(generatedProjectView||!project?.model.parts.length||libraryOpen)&&(job?.state!=='failed'||libraryOpen)&&(project?.external_tracking&&(project.files?.length??0)>1?<div className="project-library">{!!project.model.parts.length&&!generatedProjectView&&<button type="button" className="library-back" onClick={()=>setLibraryOpen(false)}>Back to model</button>}<h1>{project.name||'Project library'}</h1>{job&&['queued','running'].includes(job.state)&&<p className="library-load-status" role="status">Building selected entry…</p>}<p>Browse source entries to build geometry, or ask the assistant for a change.</p><SourceCatalog workspace={project.workspace_id} revision={project.revision} disabled={host?busy||agentSending||agentWorking:working} onChoose={(entry,builder)=>void openModelEntry(entry,builder)}/></div>:<div className="empty"><h1>Start a model</h1><p>Import a model or ask the assistant to create one.</p><div className="start-actions"><div className="start-file-actions"><button className="primary-button" disabled={working||!project} aria-label="Import model" onClick={()=>void run('import_model')}><Icon name="import" size={15}/>Import model…</button></div></div></div>)}
      {!advancedView&&modelInCanvas&&<nav className="camera-controls" aria-label="Camera views">
        <ViewCube getQuaternion={()=>viewer.current?.cameraQuaternion()??null} onView={chooseCamera}/>
        <button className="camera-fit" aria-label="Fit model in view" title="Fit model" onClick={()=>chooseCamera('fit')}><Icon name="fit" size={18}/><small>Fit</small></button>
      </nav>}
      {!project?.model.parts.length&&job?.state==='failed'&&<div className="build-empty"><strong>Model could not build</strong><p>Review the error below to continue.</p></div>}
      </div>
      {job?.state==='failed'&&!agentWorking&&!agentSending&&<BuildDiagnostics key={job.id} diagnostics={job.diagnostics} onRetry={()=>void run('rebuild')} onSetup={project?.external_tracking?()=>void setupProjectEntry():undefined}/>}
      <footer><span>Millimetres <span className="footer-dot">·</span> {generatedProjectView?0:project?.model.parts.length||0} parts</span><span className="hover-status" aria-label="Hovered geometry" ref={hoverStatus}/><span>{manipulating&&motion?.mode==='manipulate'&&!motion.playback?'Drag a moving part to slide or rotate it':'Drag to orbit'} <span className="footer-dot">·</span> ⇧ drag to pan <span className="footer-dot">·</span> Scroll to zoom</span></footer>
    </section>);
  const openChatIds=chatTabIds(chatList,activeChatId);
  const chatHostMap=Object.fromEntries(openChatIds.map(id=>[id,chatHost(id)]));
  const assistantPanel=(      <aside className={`inspector assistant-panel${imageDropDepth?' is-image-drop':''}`} id="panel-assistant-dock" aria-label="Assistant" hidden={!host&&!assistantOpen} onDragEnter={event=>imageDrag(event,'enter')} onDragOver={event=>imageDrag(event,'over')} onDragLeave={event=>imageDrag(event,'leave')} onDrop={event=>imageDrag(event,'drop')}>
        {!host&&<PanelResizeHandle panel="assistant" value={panelSizes.assistant} onChange={value=>resizePanel('assistant',value)}/>}
        {!host&&<header className="dock-heading"><button className="assistant-heading" aria-controls="panel-assistant" onClick={()=>setPanel('assistant')}><Icon name="chat" size={17}/>Assistant</button><button className="icon-button" aria-label="Hide Assistant panel" onClick={()=>{setAssistantOpen(false);requestAnimationFrame(()=>document.getElementById('tab-assistant')?.focus());}}><Icon name="close" size={16}/></button></header>}
        <AssistantChatDock
          workspaceKey={chatWorkspace||project?.workspace_id||'none'}
          chats={chatList}
          activeId={activeChatId}
          busy={!project||chatBusy||agentSending}
          pendingChatIds={pendingChatIds}
          landedChat={landedChat}
          panelOpen={assistantOpen}
          hosts={chatHostMap}
          onVisibleChats={setVisibleChatIds}
          onNew={()=>changeChat('new')}
          onSelect={id=>changeChat('select',id)}
          onClose={id=>changeChat('remove',id)}
          onRestore={id=>changeChat('restore',id)}
          renderChat={chatId=>{
            const messages=chatId===activeChatId?chat:chatMessages[chatId]||[];
            const live=!!((agentSending||agentWorking)&&(agentTurn?.chat_id?agentTurn.chat_id===chatId:chatId===activeChatId));
            const draft=draftFor(chatId);
            const activePane=chatId===activeChatId||(!activeChatId&&chatId==='new');
            return <div className="assistant-chat-pane" data-chat-id={chatId} tabIndex={-1} onPointerDown={event=>{if(!(event.target as HTMLElement).closest('button,input,textarea,select,a,summary'))event.currentTarget.focus({preventScroll:true});}}>
              <ConversationScroll key={`${project?.workspace_id}:${chatId}`} ref={activePane?conversationRef:undefined}>
                <div className="inspector-page" id={activePane?'panel-assistant':undefined} role="region" aria-label="Assistant conversation">
                  {!messages.length&&!live&&<div className="assistant-welcome"><h3>Describe a change</h3><p>Ask for a part, an edit, or a mechanism. It shows up in this document.</p></div>}
                  {messages.map((entry,index)=><div key={`${project?.workspace_id}:${chatId}:${index}`} className={`chat-message ${entry.role}`}><strong>{entry.role==='user'?'You':'Cadpad'}</strong>{entry.role==='user'?<>{editingMessage?.chatId===chatId&&editingMessage.index===index?<div className="message-edit"><textarea aria-label="Edit message" onKeyDown={event=>{if(event.key==='Escape'&&!editingMessageBusy){event.preventDefault();setEditingMessage(null);requestAnimationFrame(()=>document.getElementById('model-prompt')?.focus());}else if(event.key==='Enter'&&!event.shiftKey&&!event.nativeEvent.isComposing){event.preventDefault();if(!editingMessageBusy&&editingMessage.text.trim())void submitMessageEdit();}}} value={editingMessage.text} onChange={event=>setEditingMessage({...editingMessage,text:event.target.value})}/><p>Sending replaces this message and removes all later replies and messages.</p><button disabled={editingMessageBusy} onClick={()=>setEditingMessage(null)}>Cancel</button><button className="primary-button" disabled={editingMessageBusy||!editingMessage.text.trim()} onClick={()=>void submitMessageEdit()}>{editingMessageBusy?'Restoring…':'Send'}</button></div>:<p>{entry.text}</p>}{entry.image&&<img className="message-image" src={`data:image/png;base64,${entry.image}`} alt="Attached reference"/>}<button className="message-revert" disabled={busy||working||agentWorking||agentSending||editingMessageBusy} onClick={()=>setEditingMessage({chatId,index,text:entry.text,revision:entry.checkpoint_revision})}><Icon name="edit" size={13}/>Edit</button>{entry.checkpoint_revision&&<button className="message-revert" title="Restore the model to before this message" disabled={busy||working||agentWorking||agentSending} onClick={()=>void run('restore_checkpoint',{target:entry.checkpoint_revision})}><Icon name="undo" size={13}/>Restore</button>}</>:<FinishedReply text={entry.text} steps={entry.steps}/>}{!!entry.references?.length&&<ul className="message-references" aria-label="Referenced geometry">{entry.references.map(chip=><li key={`${chip.instance_id}:${chip.entity_id}`} className="reference-chip" title={`${chip.part} · ${chip.entity_id}${chip.file?` · ${chip.file}:${chip.line}`:''}`}>{entry.references!.length>1&&<b className="reference-letter">{chip.letter}</b>}<span className="reference-part">{chip.part}</span><span className="reference-detail">{chip.label}</span></li>)}</ul>}</div>)}
                  {activePane&&!!agentChromeStatus&&<span className="visually-hidden" data-agent-chrome-status role="status">{agentChromeStatus}</span>}
                  {live&&<div className="chat-message assistant streaming" aria-live="polite"><strong>Cadpad</strong>{agentWorking&&agentTurn.steps?.length?<LiveReply steps={agentTurn.steps} activity={agentTurn.activity} startedAt={agentTurn.started_at} stopping={agentStartCancelling||agentTurn.state==='cancelling'}/>:agentWorking&&agentTurn.text?<><Markdown text={agentTurn.text}/><LiveReply steps={[]} activity={agentTurn.activity} startedAt={agentTurn.started_at}/></>:<LiveReply steps={[]} activity={agentStartCancelling?'Stopping':agentWorking?agentTurn.activity:'Starting'} startedAt={agentWorking?agentTurn.started_at:undefined}/>}</div>}
                  {live&&agentTurn?.state==='failed'&&<p className="agent-failure">{agentTurn.error?.message||'The agent could not finish this change.'}</p>}
                  {live&&agentTurn?.state==='cancelled'&&<p className="muted">Stopped. The last committed model is still available.</p>}
                  {live&&!!agentTurn?.artifacts.length&&<section className="agent-artifacts">{agentTurn.artifacts.map(artifact=><button key={artifact.handle} onClick={()=>saveAgentArtifact(artifact)}>Save {artifact.format} export</button>)}</section>}
                </div>
              </ConversationScroll>
              <form className="prompt-form" onSubmit={event=>{event.preventDefault();setPanel('assistant');void sendPrompt(undefined,chatId==='new'?undefined:chatId);}}><div className="composer">{activePane&&selectionNotice&&<div className="composer-notice" role="status">{selectionNotice}<button type="button" className="icon-button" aria-label="Dismiss selection notice" onClick={()=>setSelectionNotice('')}><Icon name="close" size={12}/></button></div>}{activePane&&!!composerReferences.length&&<ul className="composer-references" aria-label="Selected geometry sent with your message">{referenceChips().map(chip=><li key={entityKey(chip.entity)} className={`reference-chip${chip.omitted?' is-omitted':''}`} title={`${chip.part} · ${chip.entity_id}${chip.file?` · ${chip.file}:${chip.line}`:''}${chip.omitted?' — left out of the next message':''}`}>{chip.letter&&<b className="reference-letter">{chip.letter}</b>}<span className="reference-part">{chip.part}</span><span className="reference-detail">{chip.label}{chip.file&&<> · <code>{chip.file}</code></>}</span><button type="button" className="reference-remove" aria-label={chip.omitted?`Include ${chip.part} ${chip.label}`:`Leave ${chip.part} ${chip.label} out of the message`} onClick={()=>remoteSelection?focusedSelection?.toggleOmitted(chip.entity):toggleOmitted(chip.entity)}>{chip.omitted?'↺':<Icon name="close" size={11}/>}</button></li>)}</ul>}{activePane&&attachedImage&&<div className="composer-image" aria-label="Attached reference image"><img src={attachedImage.preview} alt=""/><span title={attachedImage.name}>{attachedImage.name}</span><button type="button" className="reference-remove" aria-label="Remove attached image" onClick={()=>setAttachedImage(null)}><Icon name="close" size={11}/></button></div>}<textarea id={activePane?'model-prompt':undefined} aria-label="Describe your model" placeholder={activePane&&composerReferences.length?(composerReferences.length>1?'Ask about A, B…  (⇧-click to add or remove)':'Ask about this selection…  (⇧-click to add more)'):activePane&&attachedImage?'Ask about this image…':'Ask for a part or a change…'} value={draft} onPaste={event=>{
                const text=event.clipboardData.getData('text/plain');
                if(!text)return;
                event.preventDefault();
                const input=event.currentTarget,start=input.selectionStart,end=input.selectionEnd;
                setDraft(chatId,draft.slice(0,start)+text+draft.slice(end));
                requestAnimationFrame(()=>{input.selectionStart=input.selectionEnd=start+text.length;});
              }} onFocus={()=>{setPanel('assistant');if(chatId!==activeChatId&&chatId!=='new')void changeChat('select',chatId);}} onChange={event=>setDraft(chatId,event.target.value)} onKeyDown={event=>{if(event.key==='Enter'&&!event.shiftKey){event.preventDefault();if(!agentWorking&&!agentSending&&!chatBusy){setPanel('assistant');void sendPrompt(undefined,chatId==='new'?undefined:chatId);}}}}/><div className="composer-actions"><button type="button" className="composer-settings" onClick={()=>setProviderOpen(true)}><Icon name="sliders" size={14}/>{providerSettings?.configured===false?'Connect agent':'Agent settings'}</button>{agentWorking||agentSending?<button className="send-prompt" title="Stop assistant" aria-label="Stop assistant" disabled={agentStartCancelling||agentTurn?.state==='cancelling'} type="button" onClick={()=>void stopAgent()}><span className="stop-glyph" aria-hidden="true"/></button>:<button className="send-prompt" title="Send message" aria-label="Send message" disabled={!project||!draft.trim()||chatBusy} type="submit"><Icon name="arrowUp" size={18}/></button>}</div></div></form>
            </div>;
          }}
        />
        {!!imageDropDepth&&<div className="assistant-image-drop" aria-hidden="true">Drop image to attach</div>}
      </aside>);
  const dialogs=(<>{error&&<AppAlert message={error} active={!host||host.active} onDismiss={()=>setError('')}/>}{externalReviewOpen&&external?.changed&&<ExternalChangesDialog changes={external.changes} conflicts={external.conflicts} resolutions={externalResolutions} busy={working} onResolve={(path,value)=>setExternalResolutions(values=>({...values,[path]:value}))} onClose={()=>setExternalReviewOpen(false)} onReload={()=>{setExternalReviewOpen(false);void run('reload_project',{fingerprint:external.fingerprint,resolutions:externalResolutions});}}/>}{project&&<QuickModelSelector open={quickModelOpen} workspace={project.workspace_id} revision={project.revision} activeEntry={host?.modelEntry?.entry} activeBuilder={host?.modelEntry?.builder} busy={host?busy||agentSending||agentWorking:working} onClose={()=>setQuickModelOpen(false)} onChoose={(entry,builder)=>void openModelEntry(entry,builder)}/>}    {entrySelection&&<ProjectEntry key={entrySelection.request_id} selection={entrySelection} busy={busy} error={entryError} onOpen={(entry,adapter)=>void openSelectedEntry(entry,adapter)} onCancel={()=>void openSelectedEntry()}/>}
    <SettingsDialog
      open={providerOpen}
      dialogRef={settingsDialog}
      agentBackend={agentBackend}
      modelId={modelId}
      apiKey={apiKey}
      savingProvider={savingProvider}
      cleaningCache={cleaningCache}
      providerSettings={providerSettings}
      mcpSettings={mcpSettings}
      models={agentModels}
      onClose={()=>setProviderOpen(false)}
      onBackend={setAgentBackend}
      onModel={setModelId}
      onApiKey={setApiKey}
      onSave={()=>void saveProvider()}
      onForgetKey={()=>invoke('forget_agent_key').then(()=>{if(providerSettings)setProviderSettings({...providerSettings,configured:false});setApiKey('');}).catch(e=>setError(message(e)))}
      onRecheck={()=>invoke<AgentSettings>('agent_settings').then(setProviderSettings).catch(error=>setError(message(error)))}
      onCleanCache={()=>void cleanCache()}
      onExternalOpen={setSettingsOpen}
      onDisconnect={sessionId=>invoke('disconnect_external',{sessionId}).catch(e=>setError(message(e)))}
      onError={setError}
    /></>);
  if(host)return <>
    {host.chrome&&createPortal(headerBar,host.header)}
    {host.chrome&&createPortal(activityRail,host.rail)}
    {host.chrome&&createPortal(noticesPanel,host.notices)}
    {host.chrome&&createPortal(modelPanel,host.tools.model)}
    {host.active&&createPortal(motionPanel('move'),host.tools.move,'move')}
    {host.active&&createPortal(motionPanel('simulate'),host.tools.simulate,'simulate')}
    {host.active&&createPortal(motionPanel('test'),host.tools.test,'test')}
    {host.chrome&&createPortal(assistantPanel,host.tools.assistant)}
    {canvasPanel}{host.active&&dialogs}
  </>;
  return <div className={`app ${/Mac/.test(navigator.platform)?'macos':''}`} style={{'--model-width':`${panelSizes.model}px`,'--assistant-width':`${panelSizes.assistant}px`,'--motion-width':`${panelSizes.motion}px`} as React.CSSProperties}>
    {headerBar}
    <main>
      {activityRail}
      <div className="workbench">
        {noticesPanel}
        <div className="workbench-body">
          {modelPanel}
          {motionTask==='test'?motionPanel('test'):<div className="simulation-sections"><details open><summary>Move<small>Position parts using joints</small></summary>{motionPanel('move')}</details><details open><summary>Physics<small>Preview forces and contact</small></summary>{motionPanel('simulate')}</details></div>}
          <div className="canvas-stack">
            {canvasPanel}

          </div>
        </div>
      </div>
      {assistantPanel}
    </main>
    {dialogs}
  </div>;
}
