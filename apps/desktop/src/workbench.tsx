import React,{useEffect,useLayoutEffect,useRef,useState} from 'react';
import {Channel,invoke} from '@tauri-apps/api/core';
import {App,hasUnsavedChanges,type Project} from './app';
import {DockArea,DockContent,group,groups,placeTab,placeTabs,removeTab,updateGroup,validateDock,dockId,type DockNode,type DockTab,type DockEdge} from './dock-layout';
import {DockDragProvider,DockOuterTarget,type DockDragPayload} from './dock-drag';
import type {SessionHost,ToolTab,MessageSink,SelectionContext} from './session-host';
import {Icon} from './ui';
import {ProjectEntry,type EntrySelection} from './project-entry';
import {SidebarSections} from './sidebar-sections';
import {AppAlert} from './app-alert';
import './workbench.css';

type Area='canvas'|'left'|'right';
type DocumentTab={id:string;workspace:string;name:string;dirty:boolean;modelEntry?:{entry:string;builder:string;project:string}};
type ModelOpen={identity:string;cancelled:boolean;done:boolean;dropped:boolean;docClosed:boolean;workspace:string;jobId:string;docId:string;promise:Promise<void>};
function modelJob(job:unknown):{id:string;state:string}|undefined{
  if(!job||typeof job!=='object')return;
  const record=job as {id?:unknown;state?:unknown};
  if(typeof record.id!=='string'||typeof record.state!=='string'||!record.id)return;
  return {id:record.id,state:record.state};
}
function modelPending(project:Project,job?:{state:string}){
  const state=job?.state??project.activity.find(item=>['queued','running'].includes(item.state))?.state;
  return !!state&&['queued','running'].includes(state)&&project.model.parts.length===0;
}
type Layout={projectWorkspace?:string;canvas:DockNode;left:DockNode;right:DockNode;documents:DocumentTab[];focus:string;leftWidth:number;rightWidth:number;expanded:ToolTab[];locations:Partial<Record<ToolTab,Area>>;sectionWeights:Partial<Record<ToolTab,number>>};
const key='cadpad.dock-layout.v2';
const describeError=(error:unknown)=>error&&typeof error==='object'&&'message' in error?String(error.message):String(error);
const titleCase=(name:string)=>name.replace(/_/g,' ').replace(/^./,letter=>letter.toUpperCase());
/** Explorer entry names stay stable even when an adapter publishes a generic model root. */
function modelTabTitle(project:Project|undefined,fallback:string,entry?:{builder:string}){
  if(entry)return titleCase(entry.builder.split(':')[0]);
  // A generated view left on the project root is not a model the user opened.
  if(project?.model_entry)return fallback;
  const roots=[...new Set((project?.model.parts||[]).map(part=>part.name.split('/')[0]).filter(Boolean))];
  return roots.length===1?titleCase(roots[0]):fallback;
}
const toolTabs:Record<ToolTab,DockTab>={model:{id:'model',title:'Explorer',icon:'folder'},move:{id:'move',title:'Move',icon:'motion'},simulate:{id:'simulate',title:'Physics',icon:'physics'},test:{id:'test',title:'Requirements',icon:'tests'},assistant:{id:'assistant',title:'Assistant',icon:'chat'}};
const toolIds=Object.keys(toolTabs) as ToolTab[];
const leftTools=new Set<ToolTab>(['model','move','simulate','test']);
function fresh(project:Project):Layout{
  const tab={id:dockId(),workspace:project.workspace_id,name:modelTabTitle(project,project.name||'Untitled'),dirty:hasUnsavedChanges(project)};
  // Activity-rail tools own the pane exclusively, like VS Code view containers.
  return {projectWorkspace:project.model_parent||project.workspace_id,canvas:group([tab.id]),left:group(['model']),right:group(['assistant']),documents:[tab],focus:tab.id,leftWidth:320,rightWidth:320,expanded:['model','assistant'],locations:{},sectionWeights:{}};
}
function exclusiveStack(tabs:string[],expanded:ToolTab[],side:'left'|'right',locations:Partial<Record<ToolTab,Area>>){
  const activity=tabs.filter(id=>side==='left'?leftTools.has(id as ToolTab):id==='assistant') as ToolTab[];
  if(activity.length<=1)return {tabs,expanded,locations};
  const preferred=(expanded.find(id=>activity.includes(id))??activity[0]) as ToolTab;
  const nextLocations={...locations};
  for(const id of activity)if(id!==preferred)nextLocations[id]=side;
  const kept=tabs.filter(id=>id===preferred||!activity.includes(id as ToolTab));
  return {tabs:kept.length?kept:[preferred],expanded:expanded.filter(id=>id===preferred||!activity.includes(id)),locations:nextLocations};
}
function exclusiveDock(node:DockNode,expanded:ToolTab[],side:'left'|'right',locations:Partial<Record<ToolTab,Area>>):{node:DockNode;expanded:ToolTab[];locations:Partial<Record<ToolTab,Area>>}{
  if(node.kind==='group'){
    const norm=exclusiveStack(node.tabs,expanded,side,locations);
    return {node:{...node,tabs:norm.tabs,active:norm.tabs.includes(node.active??'')?node.active:norm.tabs[0]??null},expanded:norm.expanded,locations:norm.locations};
  }
  const first=exclusiveDock(node.first,expanded,side,locations);
  const second=exclusiveDock(node.second,first.expanded,side,first.locations);
  if(!groups(first.node).some(g=>g.tabs.length))return second;
  if(!groups(second.node).some(g=>g.tabs.length))return first;
  return {node:{...node,first:first.node,second:second.node},expanded:second.expanded,locations:second.locations};
}
function restore(project:Project,known:Set<string>):Layout{
  const fallback=fresh(project);
  try{
    const text=localStorage.getItem(key)??localStorage.getItem('cadpad.dock-layout.v1');if(!text||text.length>64000)return fallback;
    const saved=JSON.parse(text) as Layout;
    const documents=Array.isArray(saved.documents)?saved.documents.filter(doc=>typeof doc.id==='string'&&typeof doc.name==='string'&&/^[\w-]+$/.test(doc.id)&&known.has(doc.workspace)&&(saved.projectWorkspace?known.has(saved.projectWorkspace):doc.workspace===project.workspace_id)).slice(0,32):[];
    if(saved.projectWorkspace&&!documents.some(doc=>doc.workspace===project.workspace_id))return fallback;
    const canvas=validateDock(saved.canvas,new Set(documents.map(doc=>doc.id)));
    const seen=new Set<string>(),left=validateDock(saved.left,new Set(toolIds),0,seen),right=validateDock(saved.right,new Set(toolIds),0,seen);
    if(!canvas)return fallback;
    let expanded=(Array.isArray(saved.expanded)?saved.expanded:(left?groups(left):[]).concat(right?groups(right):[]).map(g=>g.active)).filter(id=>toolIds.includes(id as ToolTab)) as ToolTab[];
    let locations=saved.locations||{};
    const leftNorm=exclusiveDock(left??group(['model']),expanded,'left',locations);
    expanded=leftNorm.expanded;locations=leftNorm.locations;
    const rightNorm=exclusiveDock(right??group(['assistant']),expanded,'right',locations);
    expanded=rightNorm.expanded;locations=rightNorm.locations;
    expanded=[...new Set([...expanded,...groups(leftNorm.node).concat(groups(rightNorm.node)).filter(pane=>pane.tabs.length===1).flatMap(pane=>pane.tabs as ToolTab[])])];
    // An intentionally empty tab set is session-only; relaunch reopens the focused workspace.
    const sectionWeights=saved.sectionWeights&&typeof saved.sectionWeights==='object'?Object.fromEntries(Object.entries(saved.sectionWeights).filter(([id,value])=>toolIds.includes(id as ToolTab)&&Number.isFinite(value)&&Number(value)>0).map(([id,value])=>[id,Number(value)])) as Partial<Record<ToolTab,number>>:{};
    if(!documents.length)return {...fallback,left:leftNorm.node,right:rightNorm.node,leftWidth:Number.isFinite(saved.leftWidth)?Math.max(220,Math.min(640,saved.leftWidth)):320,rightWidth:Number.isFinite(saved.rightWidth)?Math.max(220,Math.min(640,saved.rightWidth)):320,expanded,locations,sectionWeights};
    const used=new Set(groups(canvas).flatMap(g=>g.tabs));
    const restored=documents.filter(doc=>used.has(doc.id)).map(doc=>{
      const staleView=!doc.modelEntry&&doc.workspace===project.workspace_id&&!!project.model_entry;
      return {...doc,name:staleView?project.name||'Untitled':modelTabTitle(undefined,doc.name,doc.modelEntry)};
    });if(!restored.length)return fallback;
    return {projectWorkspace:saved.projectWorkspace??project.workspace_id,canvas,left:leftNorm.node,right:rightNorm.node,documents:restored,focus:groups(canvas).some(g=>g.active===saved.focus)?saved.focus:groups(canvas).map(g=>g.active).find(Boolean)??restored[0].id,leftWidth:Number.isFinite(saved.leftWidth)?Math.max(220,Math.min(640,saved.leftWidth)):320,rightWidth:Number.isFinite(saved.rightWidth)?Math.max(220,Math.min(640,saved.rightWidth)):320,expanded,locations,sectionWeights};
  }catch{return fallback;}
}
function Slot({host,className}:{host:HTMLElement;className:string}){
  const ref=useRef<HTMLDivElement>(null);useLayoutEffect(()=>{ref.current?.appendChild(host);return()=>host.remove();},[host]);return <div ref={ref} className={className}/>;
}
function WidthDivider({side,value,onChange}:{side:'left'|'right';value:number;onChange:(width:number)=>void}){
  const start=useRef<{x:number;width:number}|null>(null);
  const resize=(width:number)=>onChange(Math.max(220,Math.min(640,window.innerWidth*.45,width)));
  return <div className="workbench-width-divider" role="separator" tabIndex={0} aria-label={`Resize ${side} sidebar`} aria-orientation="vertical" aria-valuemin={220} aria-valuemax={640} aria-valuenow={Math.round(value)} onDoubleClick={()=>onChange(320)} onPointerDown={event=>{if(event.button!==0)return;event.preventDefault();start.current={x:event.clientX,width:value};event.currentTarget.setPointerCapture(event.pointerId);}} onPointerMove={event=>{if(start.current)resize(start.current.width+(event.clientX-start.current.x)*(side==='left'?1:-1));}} onPointerUp={()=>{start.current=null;}} onPointerCancel={()=>{start.current=null;}} onLostPointerCapture={()=>{start.current=null;}} onKeyDown={event=>{const delta=event.key==='ArrowRight'?10:event.key==='ArrowLeft'?-10:0;if(delta){event.preventDefault();resize(value+delta*(side==='left'?1:-1));}if(event.key==='Enter')onChange(320);}}/>;
}
export function Workbench(){
  const [entrySelection,setEntrySelection]=useState<EntrySelection|null>(null);
  const [opening,setOpening]=useState(false);
  async function finishEntry(entry?:string,adapter?:string){
    if(!entrySelection)return;
    setOpening(true);
    try{
      const result=await invoke<{cancelled?:boolean;project:Project}>('open_project_entry',{requestId:entrySelection.request_id,entry:entry??null,...(adapter?{adapter}:{})});
      setEntrySelection(null);
      if(!result.cancelled)openProject(result.project,{replace:true});
    }catch(error){setFailure(describeError(error));}finally{setOpening(false);}
  }
  const [layout,setLayout]=useState<Layout|null>(null),[failure,setFailure]=useState('');
  const [selections,setSelections]=useState<Record<string,SelectionContext>>({});
  const [known,setKnown]=useState<{workspace_id:string;name:string;running:number;model_parent?:string}[]>([]);
  const state=useRef(layout);state.current=layout;
  const activePane=useRef<HTMLElement|null>(null);
  const chatTabs=useRef(0);
  const projects=useRef(new Map<string,Project>());
  const menus=useRef(new Map<string,MessageSink<string>>()),views=useRef(new Map<string,MessageSink<any>>());
  const menuConnected=useRef<Promise<void>|null>(null),viewConnected=useRef<Promise<string>|null>(null);
  const waitingAction=useRef<string|null>(null);
  const hosts=useRef(new Map<string,HTMLElement>());
  const mounted=useRef(new Set<string>());
  const chromeId=useRef<string|null>(null);
  const chromeDocument=useRef<{project:string;document:DocumentTab}|null>(null);
  const openingModels=useRef(new Map<string,ModelOpen>());
  const activeOpen=useRef<ModelOpen|null>(null);
  const [canvasOpening,setCanvasOpening]=useState(false);
  const memoryPolicy=useRef(Promise.resolve());
  const menuBridge=useRef<Channel<string>|null>(null),viewBridge=useRef<Channel<any>|null>(null);
  const modelViews=useRef(new Map<string,{workspace:string;sourceRevision:string}>());
  function host(id:string){if(!hosts.current.has(id)){const node=document.createElement('div');node.className='dock-content-host';hosts.current.set(id,node);}return hosts.current.get(id)!;}
  const header=host('$header'),rail=host('$rail'),notices=host('$notices');
  function pickChrome(documents:DocumentTab[],projectWorkspace:string|undefined){
    if(!documents.length){chromeDocument.current=null;return '';}
    const project=projectWorkspace||documents[0].workspace;
    // Sidebar ownership lasts for the project, independently of canvas tabs.
    if(chromeDocument.current?.project===project)return chromeDocument.current.document.id;
    const root=documents.find(doc=>doc.workspace===project&&!doc.modelEntry);
    const document=root??{id:dockId(),workspace:project,name:projects.current.get(project)?.name||'Project',dirty:false};
    chromeDocument.current={project,document};
    return document.id;
  }
  function publish(next:Layout){state.current=next;setLayout(next);}
  function update(fn:(current:Layout)=>Layout){if(state.current)publish(fn(state.current));}
  useEffect(()=>{let active=true;Promise.all([invoke<Project>('initialize'),invoke<typeof known>('workspaces')]).then(async([project,all])=>{
    if(!active)return;
    // Older builds could leave a model-view workspace focused; always restore from the project root.
    let root=project;
    if(project.model_parent){
      try{root=await invoke<Project>('project',{workspace:project.model_parent});}
      catch{if(active)setFailure(describeError('The focused model is missing its project. Open the project folder again.'));return;}
    }
    if(!active)return;
    projects.current.set(root.workspace_id,root);
    setKnown(all);
    const layout=restore(root,new Set([...all.map(ws=>ws.workspace_id),root.workspace_id]));
    // Reattach private model-view workspaces so Explorer reopen reuses them after relaunch.
    for(const doc of layout.documents){
      if(!doc.modelEntry||doc.modelEntry.project!==root.workspace_id)continue;
      modelViews.current.set(JSON.stringify([doc.modelEntry.project,doc.modelEntry.entry,doc.modelEntry.builder]),{workspace:doc.workspace,sourceRevision:root.revision});
    }
    publish(layout);
  }).catch(error=>{if(active)setFailure(describeError(error));});return()=>{active=false;};},[]);
  useEffect(()=>{if(!layout)return;const timer=setTimeout(()=>{try{localStorage.setItem(key,JSON.stringify(layout));}catch{}},200);return()=>clearTimeout(timer);},[layout]);
  useLayoutEffect(()=>{
    const pane=layout?.focus?hosts.current.get(layout.focus)?.closest<HTMLElement>('[data-dock-group]'):null;
    if(pane?.isConnected)activePane.current=pane;
  },[layout?.focus]);
  useEffect(()=>{const doc=layout?.documents.find(doc=>doc.id===layout.focus);if(doc)void invoke('switch_project',{workspace:doc.workspace}).catch(error=>setFailure(describeError(error)));},[layout?.focus]);
  useEffect(()=>{if(!layout||layout.documents.length)return;void invoke('update_native_menu',{status:{has_project:false,has_parts:false,working:false,busy:false,can_undo:false,editing:false,dimensions:false,advanced:false,document_name:'Cadpad',inspector:'',model_browser_open:false,motion_panel_open:false,has_selection:false,has_hidden_parts:false,selection_mode:'face',dimension_scope:'selected',workspaces:known.filter(ws=>!ws.model_parent).map(ws=>({id:ws.workspace_id,name:ws.name,selected:false,running:!!ws.running}))}}).catch(()=>{});},[layout?.documents.length,known]);
  function focus(area:Area,groupId:string,tab:string){
    update(current=>{
      const node=groups(current[area]).find(g=>g.id===groupId);if(node?.active===tab&&(area!=='canvas'||current.focus===tab))return current;
      return {...current,[area]:updateGroup(current[area],groupId,g=>({...g,active:tab})),focus:area==='canvas'?tab:current.focus};
    });
  }
  function openProject(project:Project,options?:{replace?:boolean}){
    projects.current.set(project.workspace_id,project);
    setKnown(current=>current.some(ws=>ws.workspace_id===project.workspace_id)?current:[...current,{workspace_id:project.workspace_id,name:project.name||'Untitled',running:0}]);
    update(current=>{
      const doc={id:dockId(),workspace:project.workspace_id,name:modelTabTitle(project,project.name||'Untitled'),dirty:hasUnsavedChanges(project)};
      if(options?.replace||!current.documents.length){
        // Reuse the sidebar host session when reopening the same project after closing its canvas tab.
        const kept=current.documents.length===1?current.documents.find(item=>item.workspace===project.workspace_id):undefined;
        if(kept)return {...current,projectWorkspace:project.workspace_id,documents:[{...kept,name:modelTabTitle(project,kept.name,kept.modelEntry),dirty:hasUnsavedChanges(project)}],canvas:group([kept.id]),focus:kept.id};
        chatTabs.current=0;return {...current,projectWorkspace:project.workspace_id,documents:[doc],canvas:group([doc.id]),focus:doc.id};
      }
      const existing=current.documents.find(item=>item.workspace===project.workspace_id);
      if(existing){const owner=groups(current.canvas).find(g=>g.tabs.includes(existing.id));return {...current,focus:existing.id,canvas:owner?updateGroup(current.canvas,owner.id,g=>({...g,active:existing.id})):placeTab(current.canvas,existing.id,groups(current.canvas)[0].id)};}
      const target=groups(current.canvas).find(g=>g.tabs.includes(current.focus))||groups(current.canvas)[0];
      const open=new Set(groups(current.canvas).flatMap(pane=>pane.tabs));
      return {...current,documents:[...current.documents.filter(item=>open.has(item.id)),doc],canvas:placeTab(current.canvas,doc.id,target.id),focus:doc.id};
    });
  }
  function focusDocument(id:string){
    const layout=state.current;if(!layout)return;
    const pane=groups(layout.canvas).find(group=>group.tabs.includes(id));
    if(pane)focus('canvas',pane.id,id);
    else update(current=>({...current,canvas:placeTab(current.canvas,id,groups(current.canvas)[0].id),focus:id}));
  }
  function closeDocument(id:string){
    update(current=>{
      if(!current.documents.some(doc=>doc.id===id))return current;
      const canvas=removeTab(current.canvas,id);
      const documents=current.documents.filter(doc=>doc.id!==id);
      const focus=current.focus===id?(groups(canvas).map(pane=>pane.active).find(Boolean)??documents[0]?.id??''):current.focus;
      return {...current,canvas,documents,focus};
    });
  }
  function releaseOpen(open:ModelOpen){
    open.cancelled=true;
    if(activeOpen.current===open)activeOpen.current=null;
    if(open.docId&&!open.docClosed){open.docClosed=true;closeDocument(open.docId);}
    if(!open.workspace||open.dropped)return;
    open.dropped=true;
    modelViews.current.delete(open.identity);
    projects.current.delete(open.workspace);
    const workspace=open.workspace,jobId=open.jobId;
    if(jobId)void invoke('stop',{workspace,jobId}).catch(()=>{});
    void invoke('forget_workspace',{workspace}).catch(()=>{});
  }
  function cancelOpen(open:ModelOpen){if(!open.done)releaseOpen(open);}
  function noteModelProgress(project:Project){
    const open=activeOpen.current;
    if(!open||open.cancelled||open.workspace!==project.workspace_id)return;
    if(!modelPending(project))open.done=true;
    setCanvasOpening(false);
    if(open.done)activeOpen.current=null;
  }
  async function openModel(document:string,entry:string,builder:string){
    const current=state.current,source=current?.documents.find(doc=>doc.id===document)
      ??(chromeDocument.current?.document.id===document?chromeDocument.current.document:undefined);
    if(!current||!source)return;
    const project=current.projectWorkspace||source.workspace;
    const root=current.documents.find(doc=>doc.workspace===project&&!doc.modelEntry)||source;
    const identity=JSON.stringify([project,entry,builder]);
    const existing=current.documents.find(doc=>doc.modelEntry?.project===project&&doc.modelEntry.entry===entry&&doc.modelEntry.builder===builder);
    if(existing){
      const flight=activeOpen.current;
      if(flight&&flight.docId!==existing.id&&!flight.done)cancelOpen(flight);
      setCanvasOpening(false);
      focusDocument(existing.id);
      return;
    }
    const pending=openingModels.current.get(identity);
    if(pending&&!pending.cancelled&&!pending.done){
      setCanvasOpening(true);
      if(pending.docId)focusDocument(pending.docId);
      return pending.promise;
    }
    const flight=activeOpen.current;
    const replacing=!!flight&&!flight.done&&flight.identity!==identity;
    if(!replacing&&current.documents.length>=32){setFailure('Close a model tab before opening another.');return;}
    const sourceProject=projects.current.get(project)||projects.current.get(root.workspace)||projects.current.get(source.workspace);
    if(!sourceProject)return;
    if(replacing&&flight)cancelOpen(flight);
    setCanvasOpening(true);
    const open:ModelOpen={identity,cancelled:false,done:false,dropped:false,docClosed:false,workspace:'',jobId:'',docId:'',promise:Promise.resolve()};
    openingModels.current.set(identity,open);
    activeOpen.current=open;
    const task=(async()=>{
      try{
        const cached=modelViews.current.get(identity);
        let result:{project:Project;job?:unknown}|undefined;
        if(cached?.sourceRevision===sourceProject.revision){
          try{result={project:await invoke<Project>('project',{workspace:cached.workspace})};}
          catch{modelViews.current.delete(identity);}
        }
        result??=await invoke<{project:Project;job:unknown}>('open_project_model',{workspace:sourceProject.workspace_id,revision:sourceProject.revision,entry,builder});
        const job=modelJob(result.job)??modelJob(result.project.activity.find(item=>['queued','running'].includes(item.state))??result.project.activity.at(-1));
        open.workspace=result.project.workspace_id;
        open.jobId=job?.id??'';
        if(open.cancelled||state.current?.projectWorkspace!==project){
          const active=activeOpen.current===open;
          releaseOpen(open);
          if(active)setCanvasOpening(false);
          return;
        }
        if(!modelPending(result.project,job))open.done=true;
        if(modelViews.current.size>=128&&!modelViews.current.has(identity))modelViews.current.delete(modelViews.current.keys().next().value!);
        modelViews.current.set(identity,{workspace:result.project.workspace_id,sourceRevision:sourceProject.revision});
        projects.current.set(result.project.workspace_id,result.project);
        const doc:DocumentTab={id:dockId(),workspace:result.project.workspace_id,name:modelTabTitle(result.project,entry,{builder}),dirty:hasUnsavedChanges(result.project),modelEntry:{entry,builder,project}};
        open.docId=doc.id;
        update(layout=>{
          if(open.cancelled||layout.projectWorkspace!==project)return layout;
          const target=groups(layout.canvas).find(group=>group.tabs.includes(layout.focus))||groups(layout.canvas)[0];
          return {...layout,documents:[...layout.documents,doc],canvas:placeTab(layout.canvas,doc.id,target.id),focus:doc.id};
        });
        if(open.cancelled)releaseOpen(open);
      }catch(error){
        if(!open.cancelled&&state.current?.projectWorkspace===project)setFailure(describeError(error));
        open.done=true;
        if(activeOpen.current===open){activeOpen.current=null;setCanvasOpening(false);}
      }finally{if(openingModels.current.get(identity)===open)openingModels.current.delete(identity);}
    })();
    open.promise=task;
    return task;
  }
  async function openWorkspace(workspace:string){try{openProject(await invoke<Project>('project',{workspace}),{replace:true});}catch(error){setFailure(describeError(error));}}
  async function forgetRecent(workspace:string){
    try{
      await invoke('forget_workspace',{workspace});
      projects.current.delete(workspace);
      setKnown(current=>current.filter(ws=>ws.workspace_id!==workspace));
      if(state.current?.documents.some(doc=>doc.workspace===workspace))update(current=>{
        let canvas=current.canvas,documents=current.documents,focus=current.focus;
        for(const doc of current.documents.filter(item=>item.workspace===workspace)){canvas=removeTab(canvas,doc.id);documents=documents.filter(item=>item.id!==doc.id);if(focus===doc.id)focus=groups(canvas).map(g=>g.active).find(Boolean)??'';}
        return {...current,canvas,documents,focus};
      });
    }catch(error){setFailure(describeError(error));}
  }
  async function clearRecent(){
    try{
      await invoke('forget_workspaces');
      projects.current.clear();
      setKnown([]);
      if(state.current&&state.current.documents.length)update(current=>({...current,canvas:group([]),documents:[],focus:''}));
    }catch(error){setFailure(describeError(error));}
  }
  function closeTab(area:Area,id:string){update(current=>{
    const tree=removeTab(current[area],id);
    const next={...current,[area]:tree};
    const canvasTabs=groups(next.canvas).flatMap(pane=>pane.tabs);
    // Keep one session alive to host the project's remaining chat tabs.
    const documents=current.documents.filter(doc=>canvasTabs.includes(doc.id));
    if(!documents.length&&chatTabs.current){
      const session=current.documents.find(doc=>doc.id===current.focus)??current.documents[0];
      if(session)documents.push(session);
    }
    const owner=groups(current.canvas).find(pane=>pane.tabs.includes(id));
    const focus=documents.some(doc=>doc.id===current.focus)?current.focus:
      groups(next.canvas).find(pane=>pane.id===owner?.id)?.active??groups(next.canvas).map(pane=>pane.active).find(Boolean)??documents[0]?.id??'';
    return {...next,documents,focus};
  });}
  function closeActiveTab(){
    const current=state.current;if(!current)return;
    const pane=activePane.current?.isConnected?activePane.current:null;
    const area=pane?.closest<HTMLElement>('[data-dock-area]')?.dataset.dockArea;
    if(area==='chats'){
      const tab=pane?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.dataset.tab;
      if(tab)pane?.dispatchEvent(new CustomEvent('cadpad-close-chat',{bubbles:true,detail:{chatId:tab}}));
      return;
    }
    if(pane&&area==='canvas'){
      const tab=groups(current.canvas).find(group=>group.id===pane.dataset.dockGroup)?.active;
      if(tab){closeTab('canvas',tab);return;}
    }else if(pane&&(area==='left'||area==='right')){
      // Activity-rail panels stay open; only their nested content tabs close.
      const chat=pane.querySelector<HTMLElement>('.dock-chats .dock-group.focused')??pane.querySelector<HTMLElement>('.dock-chats .dock-group');
      const tab=chat?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.dataset.tab;
      if(tab)chat?.dispatchEvent(new CustomEvent('cadpad-close-chat',{bubbles:true,detail:{chatId:tab}}));
      return;
    }
    const canvas=groups(current.canvas).find(pane=>pane.tabs.includes(current.focus));
    if(canvas?.active){closeTab('canvas',canvas.active);return;}
    const chat=document.querySelector<HTMLElement>('.docked-sidebar .dock-chats .dock-group.focused')??document.querySelector<HTMLElement>('.docked-sidebar .dock-chats .dock-group');
    const tab=chat?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.dataset.tab;
    if(tab)chat?.dispatchEvent(new CustomEvent('cadpad-close-chat',{bubbles:true,detail:{chatId:tab}}));
  }
  useEffect(()=>{
    const remember=(event:Event)=>{
      const pane=(event.target as Element|null)?.closest?.<HTMLElement>('[data-dock-group],[data-sidebar-section]');
      if(pane)activePane.current=pane;
    };
    const chatsChanged=(event:Event)=>{
      chatTabs.current=(event as CustomEvent<{count:number}>).detail.count;
      const current=state.current;
      if(current?.documents.length&&!chatTabs.current&&!groups(current.canvas).some(pane=>pane.tabs.length)){
        update(layout=>({...layout,documents:[],focus:''}));
      }
    };
    document.addEventListener('pointerdown',remember,true);document.addEventListener('focusin',remember,true);
    document.addEventListener('cadpad-chat-tabs-change',chatsChanged);
    return()=>{document.removeEventListener('pointerdown',remember,true);document.removeEventListener('focusin',remember,true);document.removeEventListener('cadpad-chat-tabs-change',chatsChanged);};
  },[]);
  useEffect(()=>{
    if(!layout)return;
    const tabs=groups(layout.left).flatMap(group=>group.tabs);
    if(tabs.includes('move')&&!tabs.includes('simulate'))update(current=>({...current,left:group(['move','simulate']),expanded:current.expanded.filter(id=>id!=='simulate')}));
  },[layout]);
  function toggleSection(tool:ToolTab){update(current=>({...current,expanded:current.expanded.includes(tool)?current.expanded.filter(id=>id!==tool):[...current.expanded,tool]}));}
  function showTool(tool:ToolTab,toggle=false){
    if(tool==='move'||tool==='simulate'){
      update(current=>{
        const shown=groups(current.left).some(pane=>pane.tabs.includes('move'));
        if(toggle&&shown)return {...current,left:group(),expanded:current.expanded.filter(id=>id!=='move'&&id!=='simulate')};
        return {...current,left:group(['move','simulate']),expanded:[...current.expanded.filter(id=>!leftTools.has(id)),tool]};
      });return;
    }
    update(current=>{
    const home: 'left'|'right'=tool==='assistant'?'right':'left';
    const remembered=current.locations[tool]==='left'||current.locations[tool]==='right'?current.locations[tool] as 'left'|'right':home;
    let area:'left'|'right'|null=null;
    for(const side of ['left','right'] as const)if(groups(current[side]).some(g=>g.tabs.includes(tool))){area=side;break;}
    const shown=!!area&&current.expanded.includes(tool);
    if(toggle&&shown&&area)return {...current,[area]:removeTab(current[area],tool),expanded:current.expanded.filter(id=>id!==tool),locations:{...current.locations,[tool]:area}};
    const target=area??remembered,other: 'left'|'right'=target==='left'?'right':'left';
    const targetTabs=groups(current[target]).flatMap(g=>g.tabs);
    if(targetTabs.length===1&&targetTabs[0]===tool)return current.expanded.includes(tool)?current:{...current,expanded:[...current.expanded,tool]};
    const displaced=targetTabs.filter(id=>id!==tool) as ToolTab[];
    const otherTree=groups(current[other]).some(g=>g.tabs.includes(tool))?removeTab(current[other],tool):current[other];
    const locations={...current.locations,[tool]:target,...Object.fromEntries(displaced.map(id=>[id,target]))};
    const expanded=[...current.expanded.filter(id=>id!==tool&&!displaced.includes(id as ToolTab)&&groups(otherTree).some(g=>g.tabs.includes(id))),tool] as ToolTab[];
    return {...current,[target]:group([tool],tool),[other]:otherTree,expanded,locations};
  });}
  function drop(area:Area,tab:string,source:string,target:string,edge:DockEdge,before?:string,payload?:DockDragPayload){
    if(area==='canvas'&&source!=='canvas'||area!=='canvas'&&source==='canvas')return;
    update(current=>{
      const incoming=(payload?.tabs||[tab]).filter(id=>area==='canvas'?current.documents.some(doc=>doc.id===id):toolIds.includes(id as ToolTab));
      if(!incoming.length||!groups(current[area]).some(g=>g.id===target))return current;
      const active=incoming.includes(tab)?tab:incoming[0];
      const owner=groups(current[area]).find(g=>g.id===target)!;
      // Dragging the only model tab to an edge creates a second camera view.
      if(area==='canvas'&&edge!=='center'&&payload?.kind!=='group'&&incoming.length===1&&owner.tabs.length===1&&owner.tabs[0]===active){
        const original=current.documents.find(doc=>doc.id===active);if(!original||current.documents.length>=32)return current;
        const duplicate={...original,id:dockId()};
        return {...current,documents:[...current.documents,duplicate],canvas:placeTab(current.canvas,duplicate.id,target,edge),focus:duplicate.id};
      }
      let next={...current,[area]:placeTabs(current[area],incoming,target,edge,edge==='center'?before:undefined,active)};
      if(area!=='canvas'){
        const other=area==='left'?'right':'left';
        const tools=incoming.filter(id=>toolIds.includes(id as ToolTab)) as ToolTab[];
        next={...next,[other]:incoming.reduce((tree,id)=>removeTab(tree,id),current[other]),locations:{...current.locations,...Object.fromEntries(incoming.map(id=>[id,area]))},expanded:[...new Set([...current.expanded,...tools])]};
      }else next.focus=active;
      return next;
    });
  }

  function split(area:Area,groupId:string,axis:'horizontal'|'vertical'){
    update(current=>{
      const owner=groups(current[area]).find(g=>g.id===groupId);if(!owner?.active)return current;
      const edge=axis==='horizontal'?'right':'bottom';
      if(area==='canvas'){
        const source=current.documents.find(doc=>doc.id===owner.active);if(!source||current.documents.length>=32)return current;
        const doc={...source,id:dockId()};return {...current,documents:[...current.documents,doc],canvas:placeTab(current.canvas,doc.id,groupId,edge),focus:doc.id};
      }
      return current;
    });
  }
  async function action(command:string){
    const current=state.current;if(!current)return;
    const resolved=command==='new'?'new-model':command;
    const menu=menus.current.get(current.focus);
    if(menu){menu.onmessage(resolved);return;}
    if(!['new-model','new-project','open','settings'].includes(resolved))return;
    if(resolved==='open'){
      if(opening)return;
      setOpening(true);
      try{
        const result=await invoke<{cancelled?:boolean;choose_entry?:EntrySelection;project:Project}>('open_project');
        if(result.choose_entry)setEntrySelection(result.choose_entry);
        else if(!result.cancelled)openProject(result.project,{replace:true});
      }catch(error){setFailure(describeError(error));}finally{setOpening(false);}
      return;
    }
    const createModel=resolved==='new-model'||resolved==='new-project';
    waitingAction.current=createModel?null:resolved;
    try{
      const draft=await invoke<Project>('new_scratchpad');
      if(resolved==='new-project'){
        const created=await invoke<Project&{cancelled?:boolean}>('save_project',{workspace:draft.workspace_id,revision:draft.revision,saveAs:true});
        if(!created.cancelled)openProject(created);
      }else openProject(draft);
    }catch(error){waitingAction.current=null;setFailure(describeError(error));}
  }
  function connectMenu(id:string,channel:MessageSink<string>){
    menus.current.set(id,channel);
    const connection=ensureMenu();
    if(waitingAction.current&&state.current?.focus===id){const pending=waitingAction.current;waitingAction.current=null;queueMicrotask(()=>channel.onmessage(pending));}
    return connection;
  }
  function ensureMenu(){
    if(!menuConnected.current){const bridge=menuBridge.current=new Channel<string>();bridge.onmessage=command=>{
      if(command==='close-tab'){closeActiveTab();return;}
      if(command==='split-right'||command==='split-down'){const current=state.current;const owner=current?groups(current.canvas).find(g=>g.tabs.includes(current.focus)):null;if(owner)split('canvas',owner.id,command==='split-right'?'horizontal':'vertical');return;}
      if(command.startsWith('workspace:')){void openWorkspace(command.slice(10));return;}
      const active=state.current?.focus;const target=active?menus.current.get(active):null;if(target)target.onmessage(command);else void action(command);
    };menuConnected.current=invoke<void>('connect_native_menu',{channel:bridge});}
    return menuConnected.current;
  }
  useEffect(()=>{if(layout)void ensureMenu().catch(error=>setFailure(describeError(error)));},[!!layout]);
  useEffect(()=>{const save=()=>{if(state.current)try{localStorage.setItem(key,JSON.stringify(state.current));}catch{}};window.addEventListener('pagehide',save);return()=>{window.removeEventListener('pagehide',save);save();};},[]);
  function connectView(id:string,channel:MessageSink<any>){
    views.current.set(id,channel);
    if(!viewConnected.current){const bridge=viewBridge.current=new Channel<any>();bridge.onmessage=request=>{
      const current=state.current;
      const visible=new Set(current?groups(current.canvas).map(g=>g.active):[]);
      const candidates=current?.documents.filter(doc=>doc.workspace===request.workspace_id&&visible.has(doc.id))||[];
      const doc=candidates.find(doc=>doc.id===current?.focus)||candidates[0];
      const target=doc?views.current.get(doc.id):null;
      if(target)target.onmessage(request);
      else void viewConnected.current?.then(token=>invoke('complete_view',{token,requestId:request.id,reply:{error:{code:'APP_UNAVAILABLE',message:'Show the requested model in a canvas pane first'}}}));
    };viewConnected.current=invoke<string>('connect_view',{channel:bridge});}
    return viewConnected.current;
  }
  useEffect(()=>{
    const shortcuts=(event:KeyboardEvent)=>{if(!(event.metaKey||event.ctrlKey)||!state.current)return;if(event.code==='Backslash'||event.key==='\\'||event.key==='|'){event.preventDefault();const owner=groups(state.current.canvas).find(g=>g.tabs.includes(state.current!.focus));if(owner)split('canvas',owner.id,event.shiftKey?'vertical':'horizontal');}else if(event.ctrlKey&&event.key==='Tab'||event.metaKey&&event.shiftKey&&['BracketLeft','BracketRight'].includes(event.code)){event.preventDefault();const owner=groups(state.current.canvas).find(g=>g.tabs.includes(state.current!.focus));if(owner){const offset=event.key==='Tab'?(event.shiftKey?-1:1):event.code==='BracketLeft'?-1:1;focus('canvas',owner.id,owner.tabs[(owner.tabs.indexOf(state.current.focus)+offset+owner.tabs.length)%owner.tabs.length]);}}else if(event.key.toLowerCase()==='w'){event.preventDefault();closeActiveTab();}};
    window.addEventListener('keydown',shortcuts);return()=>window.removeEventListener('keydown',shortcuts);
  },[]);
  useEffect(()=>()=>{
    if(menuBridge.current)menuBridge.current.onmessage=()=>{};
    const view=viewBridge.current;
    if(view){view.onmessage=()=>{};void viewConnected.current?.then(()=>invoke('disconnect_stream',{subscriptionId:view.id})).catch(()=>{});}
    menus.current.clear();views.current.clear();projects.current.clear();modelViews.current.clear();hosts.current.clear();mounted.current.clear();
  },[]);
  const visibleWorkspaces=layout?JSON.stringify([...new Set(groups(layout.canvas).flatMap(pane=>layout.documents.filter(doc=>doc.id===pane.active).map(doc=>doc.workspace)))]):null;
  useEffect(()=>{
    if(visibleWorkspaces===null)return;
    // Serialize updates so a slow IPC reply cannot restore an older visibility policy.
    memoryPolicy.current=memoryPolicy.current.catch(()=>{}).then(()=>invoke<void>('retain_workspace_memory',{workspaces:JSON.parse(visibleWorkspaces)}));
  },[visibleWorkspaces]);
  // Release closed sessions once they no longer host any model or chat tabs.
  useEffect(()=>{
    const open=new Set(layout?.documents.map(doc=>doc.id));
    if(chromeDocument.current&&chromeDocument.current.project===layout?.projectWorkspace&&layout?.documents.length)open.add(chromeDocument.current.document.id);
    const workspaces=new Set(layout?.documents.map(doc=>doc.workspace));
    for(const id of menus.current.keys())if(!open.has(id)){menus.current.delete(id);views.current.delete(id);}
    for(const id of hosts.current.keys())if(!id.startsWith('$')&&!toolIds.includes(id as ToolTab)&&!open.has(id))hosts.current.delete(id);
    for(const id of mounted.current)if(!open.has(id))mounted.current.delete(id);
    setSelections(current=>{
      if(Object.keys(current).every(workspace=>workspaces.has(workspace)||workspace===layout?.projectWorkspace))return current;
      return Object.fromEntries(Object.entries(current).filter(([workspace])=>workspaces.has(workspace)||workspace===layout?.projectWorkspace));
    });
    // Closed model views can hold large scene metadata; reopening fetches a fresh project.
    for(const workspace of projects.current.keys())if(!workspaces.has(workspace)&&workspace!==layout?.projectWorkspace)projects.current.delete(workspace);
  },[layout?.documents]);
  if(!layout)return <div className="cadpad-startup" role="status">{failure||'Opening workspace…'}</div>;
  const activeGroups=groups(layout.canvas),visible=new Set(activeGroups.map(g=>g.active));
  const visibleTools=layout.expanded.filter(id=>groups(layout.left).concat(groups(layout.right)).some(g=>g.tabs.includes(id)));
  const docs=Object.fromEntries(layout.documents.map(doc=>[doc.id,{id:doc.id,title:doc.name,dirty:doc.dirty,icon:'cube' as const}]));
  const toolHosts=Object.fromEntries(toolIds.map(id=>[id,host(id)])) as Record<ToolTab,HTMLElement>;
  const canvasHosts=Object.fromEntries(layout.documents.map(doc=>[doc.id,host(doc.id)]));
  const focusDoc=layout.documents.find(doc=>doc.id===layout.focus);
  const projectWorkspace=layout.projectWorkspace||focusDoc?.workspace||'';
  const focusWorkspace=focusDoc?.workspace||projectWorkspace;
  chromeId.current=pickChrome(layout.documents,layout.projectWorkspace);
  const area=(name:Area)=>{
    if(name==='canvas')return <DockArea area="canvas" tree={layout.canvas} tabs={docs} hosts={canvasHosts} focused={activeGroups.find(g=>g.tabs.includes(layout.focus))?.id} onFocus={(group,tab)=>focus('canvas',group,tab)} onChange={tree=>update(current=>({...current,canvas:tree}))} onDrop={(tab,source,group,edge,before)=>drop('canvas',tab,source,group,edge,before)} onClose={id=>closeTab('canvas',id)}/>;
    const tree=layout[name],panes=groups(tree).filter(g=>g.tabs.length);
    // Split sidebars use the same directional docking chrome as the canvas.
    if(tree.kind==='split'||panes.length>1)return <DockArea area={name} tree={tree} tabs={toolTabs} hosts={toolHosts} onFocus={(groupId,tab)=>{focus(name,groupId,tab);update(current=>current.expanded.includes(tab as ToolTab)?current:{...current,expanded:[...current.expanded,tab as ToolTab]});}} onChange={next=>update(current=>({...current,[name]:next}))} onDrop={(tab,source,group,edge,before)=>drop(name,tab,source,group,edge,before)} onClose={id=>closeTab(name,id)}/>;
    const pane=panes[0]??groups(tree)[0];
    return <SidebarSections area={name} group={pane} tabs={toolTabs} hosts={toolHosts} expanded={layout.expanded} weights={layout.sectionWeights} onToggle={id=>toggleSection(id as ToolTab)} onDrop={(tab,source,before)=>drop(name,tab,source,pane.id,'center',before)} onResize={next=>update(current=>({...current,sectionWeights:{...current.sectionWeights,...next}}))}/>;
  };

  return <DockDragProvider onDrop={(payload,target)=>{if(['canvas','left','right'].includes(target.area))drop(target.area as Area,payload.active,payload.area,target.group,target.edge,target.before,payload);}}><div className={`docked-workbench ${/Mac/.test(navigator.platform)?'macos':''}`}>
    {entrySelection&&<ProjectEntry selection={entrySelection} busy={opening} error={failure} onOpen={(entry,adapter)=>void finishEntry(entry,adapter)} onCancel={()=>void finishEntry()}/> }
    <Slot host={header} className="workbench-header-slot"/>
    {!layout.documents.length&&<header className="app-header workbench-empty-header" data-tauri-drag-region/>}
    <div className="docked-main">{!groups(layout.left).some(g=>g.tabs.length)&&<DockOuterTarget area="left" group={groups(layout.left)[0].id}/>} {!groups(layout.right).some(g=>g.tabs.length)&&<DockOuterTarget area="right" group={groups(layout.right)[0].id}/>}<Slot host={rail} className="workbench-rail-slot"/>
      {!!layout.documents.length&&groups(layout.left).some(g=>g.tabs.length)&&<><div className="docked-sidebar left" style={{width:layout.leftWidth}}>{area('left')}</div><WidthDivider side="left" value={layout.leftWidth} onChange={width=>update(current=>({...current,leftWidth:width}))}/></>}
      <div className="docked-center"><Slot host={notices} className="workbench-notices-slot"/>{failure&&<AppAlert message={failure} onDismiss={()=>setFailure('')}/>}{!!layout.documents.length&&area('canvas')}{!layout.documents.length&&<div className="dock-welcome"><div className="dock-welcome-card"><h1>Cadpad</h1><p>Create a project, open a project folder, or continue a recent project.</p><div className="dock-welcome-actions"><button onClick={()=>void action('new-project')}><Icon name="plus" size={15}/>New project…</button><button className="primary-button" onClick={()=>void action('open')}><Icon name="folder" size={15}/>Open project…</button></div>{!!known.length&&<div className="dock-welcome-recent"><div className="dock-welcome-recent-heading"><h2>Recent</h2><button type="button" className="subtle-button" onClick={()=>void clearRecent()}>Clear all</button></div><ul>{known.filter(ws=>!ws.model_parent).slice(0,8).map(ws=><li key={ws.workspace_id}><button type="button" className="dock-welcome-open" onClick={()=>void openWorkspace(ws.workspace_id)}><Icon name="cube" size={16}/><span>{ws.name||'Untitled'}</span>{!!ws.running&&<small>Working</small>}</button><button type="button" className="icon-button" aria-label={`Remove ${ws.name||'Untitled'} from recent`} onClick={()=>void forgetRecent(ws.workspace_id)}><Icon name="close" size={14}/></button></li>)}</ul></div>}</div></div>}{canvasOpening&&<div className="canvas-model-opening"><div className="empty model-opening" role="status"><span className="model-opening-spinner" aria-hidden="true"/><h1>Opening model…</h1><p>Building geometry for this tab.</p></div></div>}</div>
      {!!layout.documents.length&&groups(layout.right).some(g=>g.tabs.length)&&<><WidthDivider side="right" value={layout.rightWidth} onChange={width=>update(current=>({...current,rightWidth:width}))}/><div className="docked-sidebar right" style={{width:layout.rightWidth}}>{area('right')}</div></>}
    </div>
    {[...layout.documents,...(chromeDocument.current&&!layout.documents.some(doc=>doc.id===chromeId.current)?[chromeDocument.current.document]:[])].filter(doc=>{
      if(visible.has(doc.id)||doc.id===chromeId.current)mounted.current.add(doc.id);
      return mounted.current.has(doc.id);
    }).map(doc=>{
      const sessionHost:SessionHost={
        workspaceId:doc.workspace,modelEntry:doc.modelEntry,onOpenModel:(entry,builder)=>openModel(doc.id,entry,builder),
        active:doc.id===layout.focus,chrome:doc.id===chromeId.current,visible:visible.has(doc.id),
        projectWorkspace,focusWorkspace,focusModelEntry:focusDoc?.modelEntry,focusSelection:selections[focusWorkspace],
        onSelection:context=>setSelections(current=>({...current,[context.workspace]:context})),initialProject:projects.current.get(doc.workspace),
        header,rail,notices,tools:toolHosts,visibleTools,showTool,onOpen:openProject,onOpenWorkspace:workspace=>void openWorkspace(workspace),
        onProject:project=>{noteModelProgress(project);projects.current.set(project.workspace_id,project);update(current=>{const prior=current.documents.find(item=>item.id===doc.id);const fallback=prior?.modelEntry?prior.name:project.name||'Untitled';const name=modelTabTitle(project,fallback,prior?.modelEntry);if(prior&&prior.name===name&&prior.dirty===hasUnsavedChanges(project))return current;return {...current,documents:current.documents.map(item=>item.workspace===project.workspace_id?{...item,name:modelTabTitle(project,item.modelEntry?item.name:project.name||'Untitled',item.modelEntry),dirty:hasUnsavedChanges(project)}:item)};});},
        connectMenu:channel=>connectMenu(doc.id,channel),connectView:channel=>connectView(doc.id,channel),
      };
      return <DockContent key={doc.id} host={host(doc.id)}><App host={sessionHost}/></DockContent>;
    })}
  </div></DockDragProvider>;
}
