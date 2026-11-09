import React,{useEffect,useLayoutEffect,useRef,useState} from 'react';
import {Channel,invoke} from '@tauri-apps/api/core';
import {App,type Project} from './app';
import {DockArea,DockContent,group,groups,placeTab,placeTabs,removeTab,updateGroup,validateDock,dockId,type DockNode,type DockTab,type DockEdge} from './dock-layout';
import {DockDragProvider,DockOuterTarget,type DockDragPayload} from './dock-drag';
import type {SessionHost,ToolTab} from './session-host';
import {Icon} from './ui';
import {ProjectEntry,type EntrySelection} from './project-entry';
import {SidebarSections} from './sidebar-sections';
import './workbench.css';

type Area='canvas'|'left'|'right';
type DocumentTab={id:string;workspace:string;name:string;dirty:boolean};
type Layout={canvas:DockNode;left:DockNode;right:DockNode;documents:DocumentTab[];focus:string;leftWidth:number;rightWidth:number;expanded:ToolTab[];locations:Partial<Record<ToolTab,Area>>;sectionWeights:Partial<Record<ToolTab,number>>};
const key='cadpad.dock-layout.v2';
const describeError=(error:unknown)=>error&&typeof error==='object'&&'message' in error?String(error.message):String(error);
const toolTabs:Record<ToolTab,DockTab>={model:{id:'model',title:'Explorer',icon:'folder'},move:{id:'move',title:'Move',icon:'motion'},simulate:{id:'simulate',title:'Physics',icon:'physics'},test:{id:'test',title:'Requirements',icon:'tests'},assistant:{id:'assistant',title:'Assistant',icon:'chat'}};
const toolIds=Object.keys(toolTabs) as ToolTab[];
const leftTools=new Set<ToolTab>(['model','move','simulate','test']);
function fresh(project:Project):Layout{
  const tab={id:dockId(),workspace:project.workspace_id,name:project.name||'Untitled',dirty:project.dirty};
  // Activity-rail tools own the pane exclusively, like VS Code view containers.
  return {canvas:group([tab.id]),left:group(['model']),right:group(['assistant']),documents:[tab],focus:tab.id,leftWidth:320,rightWidth:320,expanded:['model','assistant'],locations:{},sectionWeights:{}};
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
    const documents=Array.isArray(saved.documents)?saved.documents.filter(doc=>typeof doc.id==='string'&&typeof doc.name==='string'&&/^[\w-]+$/.test(doc.id)&&known.has(doc.workspace)).slice(0,32):[];
    const canvas=validateDock(saved.canvas,new Set(documents.map(doc=>doc.id)));
    const seen=new Set<string>(),left=validateDock(saved.left,new Set(toolIds),0,seen),right=validateDock(saved.right,new Set(toolIds),0,seen);
    if(!canvas)return fallback;
    let expanded=(Array.isArray(saved.expanded)?saved.expanded:(left?groups(left):[]).concat(right?groups(right):[]).map(g=>g.active)).filter(id=>toolIds.includes(id as ToolTab)) as ToolTab[];
    let locations=saved.locations||{};
    const leftNorm=exclusiveDock(left??group(['model']),expanded,'left',locations);
    expanded=leftNorm.expanded;locations=leftNorm.locations;
    const rightNorm=exclusiveDock(right??group(['assistant']),expanded,'right',locations);
    expanded=rightNorm.expanded;locations=rightNorm.locations;
    // An intentionally empty tab set is session-only; relaunch reopens the focused workspace.
    const sectionWeights=saved.sectionWeights&&typeof saved.sectionWeights==='object'?Object.fromEntries(Object.entries(saved.sectionWeights).filter(([id,value])=>toolIds.includes(id as ToolTab)&&Number.isFinite(value)&&Number(value)>0).map(([id,value])=>[id,Number(value)])) as Partial<Record<ToolTab,number>>:{};
    if(!documents.length)return {...fallback,left:leftNorm.node,right:rightNorm.node,leftWidth:Number.isFinite(saved.leftWidth)?Math.max(220,Math.min(640,saved.leftWidth)):320,rightWidth:Number.isFinite(saved.rightWidth)?Math.max(220,Math.min(640,saved.rightWidth)):320,expanded,locations,sectionWeights};
    const used=new Set(groups(canvas).flatMap(g=>g.tabs));
    const restored=documents.filter(doc=>used.has(doc.id));if(!restored.length)return fallback;
    return {canvas,left:leftNorm.node,right:rightNorm.node,documents:restored,focus:groups(canvas).some(g=>g.active===saved.focus)?saved.focus:groups(canvas).map(g=>g.active).find(Boolean)??restored[0].id,leftWidth:Number.isFinite(saved.leftWidth)?Math.max(220,Math.min(640,saved.leftWidth)):320,rightWidth:Number.isFinite(saved.rightWidth)?Math.max(220,Math.min(640,saved.rightWidth)):320,expanded,locations,sectionWeights};
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
      if(!result.cancelled)openProject(result.project);
    }catch(error){setFailure(describeError(error));}finally{setOpening(false);}
  }
  const [layout,setLayout]=useState<Layout|null>(null),[failure,setFailure]=useState('');
  const [known,setKnown]=useState<{workspace_id:string;name:string;running:number}[]>([]);
  const state=useRef(layout);state.current=layout;
  const projects=useRef(new Map<string,Project>());
  const menus=useRef(new Map<string,Channel<string>>()),views=useRef(new Map<string,Channel<any>>());
  const menuConnected=useRef<Promise<void>|null>(null),viewConnected=useRef<Promise<string>|null>(null);
  const waitingAction=useRef<string|null>(null);
  const hosts=useRef(new Map<string,HTMLElement>());
  const mounted=useRef(new Set<string>());
  function host(id:string){if(!hosts.current.has(id)){const node=document.createElement('div');node.className='dock-content-host';hosts.current.set(id,node);}return hosts.current.get(id)!;}
  const header=host('$header'),rail=host('$rail'),notices=host('$notices');
  function publish(next:Layout){state.current=next;setLayout(next);}
  function update(fn:(current:Layout)=>Layout){if(state.current)publish(fn(state.current));}
  useEffect(()=>{let active=true;Promise.all([invoke<Project>('initialize'),invoke<typeof known>('workspaces')]).then(([project,all])=>{if(!active)return;projects.current.set(project.workspace_id,project);setKnown(all);publish(restore(project,new Set([...all.map(ws=>ws.workspace_id),project.workspace_id])));}).catch(error=>{if(active)setFailure(describeError(error));});return()=>{active=false;};},[]);
  useEffect(()=>{if(!layout)return;const timer=setTimeout(()=>{try{localStorage.setItem(key,JSON.stringify(layout));}catch{}},200);return()=>clearTimeout(timer);},[layout]);
  useEffect(()=>{const doc=layout?.documents.find(doc=>doc.id===layout.focus);if(doc)void invoke('switch_project',{workspace:doc.workspace}).catch(error=>setFailure(describeError(error)));},[layout?.focus]);
  useEffect(()=>{if(!layout||layout.documents.length)return;void invoke('update_native_menu',{status:{has_project:false,has_parts:false,working:false,busy:false,can_undo:false,editing:false,dimensions:false,advanced:false,document_name:'Cadpad',inspector:'',model_browser_open:false,motion_panel_open:false,has_selection:false,has_hidden_parts:false,selection_mode:'face',dimension_scope:'selected',workspaces:known.map(ws=>({id:ws.workspace_id,name:ws.name,selected:false,running:!!ws.running}))}}).catch(()=>{});},[layout?.documents.length,known]);
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
      const doc={id:dockId(),workspace:project.workspace_id,name:project.name||'Untitled',dirty:project.dirty};
      if(options?.replace||!current.documents.length)return {...current,documents:[doc],canvas:group([doc.id]),focus:doc.id};
      const existing=current.documents.find(item=>item.workspace===project.workspace_id);
      if(existing){const owner=groups(current.canvas).find(g=>g.tabs.includes(existing.id));return {...current,focus:existing.id,canvas:owner?updateGroup(current.canvas,owner.id,g=>({...g,active:existing.id})):current.canvas};}
      const target=groups(current.canvas).find(g=>g.tabs.includes(current.focus))||groups(current.canvas)[0];
      return {...current,documents:[...current.documents,doc],canvas:placeTab(current.canvas,doc.id,target.id),focus:doc.id};
    });
  }
  async function openWorkspace(workspace:string){try{openProject(await invoke<Project>('project',{workspace}));}catch(error){setFailure(describeError(error));}}
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
    if(area!=='canvas')return {...current,[area]:tree};
    const owner=groups(current.canvas).find(g=>g.tabs.includes(id));
    const documents=current.documents.filter(doc=>doc.id!==id),focus=current.focus===id?groups(tree).find(g=>g.id===owner?.id)?.active??groups(tree).map(g=>g.active).find(Boolean)??'':current.focus;
    return {...current,canvas:tree,documents,focus};
  });}
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
        else if(!result.cancelled)openProject(result.project);
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
  function connectMenu(id:string,channel:Channel<string>){
    menus.current.set(id,channel);
    const connection=ensureMenu();
    if(waitingAction.current&&state.current?.focus===id){const pending=waitingAction.current;waitingAction.current=null;queueMicrotask(()=>channel.onmessage(pending));}
    return connection;
  }
  function ensureMenu(){
    if(!menuConnected.current){const bridge=new Channel<string>();bridge.onmessage=command=>{
      if(command==='close-tab'){
        const pane=document.activeElement?.closest<HTMLElement>('.assistant-chat-pane');
        const chatTab=document.activeElement?.closest<HTMLElement>('.assistant-chat-dock [data-tab]');
        if(chatTab){document.dispatchEvent(new CustomEvent('cadpad-close-chat',{detail:{chatId:chatTab.dataset.tab}}));return;}
        if(pane){pane.dispatchEvent(new CustomEvent('cadpad-close-chat',{bubbles:true,detail:{chatId:pane.dataset.chatId}}));return;}
        if(state.current?.focus)closeTab('canvas',state.current.focus);return;
      }
      if(command==='split-right'||command==='split-down'){const current=state.current;const owner=current?groups(current.canvas).find(g=>g.tabs.includes(current.focus)):null;if(owner)split('canvas',owner.id,command==='split-right'?'horizontal':'vertical');return;}
      if(command.startsWith('workspace:')){void openWorkspace(command.slice(10));return;}
      const active=state.current?.focus;const target=active?menus.current.get(active):null;if(target)target.onmessage(command);else void action(command);
    };menuConnected.current=invoke<void>('connect_native_menu',{channel:bridge});}
    return menuConnected.current;
  }
  useEffect(()=>{if(layout)void ensureMenu().catch(error=>setFailure(describeError(error)));},[!!layout]);
  useEffect(()=>{const save=()=>{if(state.current)try{localStorage.setItem(key,JSON.stringify(state.current));}catch{}};window.addEventListener('pagehide',save);return()=>{window.removeEventListener('pagehide',save);save();};},[]);
  function connectView(id:string,channel:Channel<any>){
    views.current.set(id,channel);
    if(!viewConnected.current){const bridge=new Channel<any>();bridge.onmessage=request=>{
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
    const shortcuts=(event:KeyboardEvent)=>{if(!(event.metaKey||event.ctrlKey)||!state.current)return;if(event.code==='Backslash'||event.key==='\\'||event.key==='|'){event.preventDefault();const owner=groups(state.current.canvas).find(g=>g.tabs.includes(state.current!.focus));if(owner)split('canvas',owner.id,event.shiftKey?'vertical':'horizontal');}else if(event.ctrlKey&&event.key==='Tab'||event.metaKey&&event.shiftKey&&['BracketLeft','BracketRight'].includes(event.code)){event.preventDefault();const owner=groups(state.current.canvas).find(g=>g.tabs.includes(state.current!.focus));if(owner){const offset=event.key==='Tab'?(event.shiftKey?-1:1):event.code==='BracketLeft'?-1:1;focus('canvas',owner.id,owner.tabs[(owner.tabs.indexOf(state.current.focus)+offset+owner.tabs.length)%owner.tabs.length]);}}else if(event.key.toLowerCase()==='w'){event.preventDefault();if(state.current.focus)closeTab('canvas',state.current.focus);}};
    window.addEventListener('keydown',shortcuts);return()=>window.removeEventListener('keydown',shortcuts);
  },[]);
  // Closed tabs release their WebGL session and routing registrations, never their workspace.
  useEffect(()=>{const open=new Set(layout?.documents.map(doc=>doc.id));for(const id of menus.current.keys())if(!open.has(id)){menus.current.delete(id);views.current.delete(id);}for(const id of hosts.current.keys())if(!id.startsWith('$')&&!toolIds.includes(id as ToolTab)&&!open.has(id))hosts.current.delete(id);},[layout?.documents]);
  if(!layout)return <div className="cadpad-startup" role="status">{failure||'Opening workspace…'}</div>;
  const activeGroups=groups(layout.canvas),visible=new Set(activeGroups.map(g=>g.active));
  const visibleTools=layout.expanded.filter(id=>groups(layout.left).concat(groups(layout.right)).some(g=>g.tabs.includes(id)));
  const docs=Object.fromEntries(layout.documents.map(doc=>[doc.id,{id:doc.id,title:doc.name,dirty:doc.dirty,icon:'cube' as const}]));
  const toolHosts=Object.fromEntries(toolIds.map(id=>[id,host(id)])) as Record<ToolTab,HTMLElement>;
  const canvasHosts=Object.fromEntries(layout.documents.map(doc=>[doc.id,host(doc.id)]));
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
      <div className="docked-center"><Slot host={notices} className="workbench-notices-slot"/>{failure&&<div role="alert" className="dock-error">{failure}<button onClick={()=>setFailure('')}>Dismiss</button></div>}{!!layout.documents.length&&area('canvas')}{!layout.documents.length&&<div className="dock-welcome"><div className="dock-welcome-card"><h1>Cadpad</h1><p>Create a project, open a project folder, or continue a recent project.</p><div className="dock-welcome-actions"><button onClick={()=>void action('new-project')}><Icon name="plus" size={15}/>New project…</button><button className="primary-button" onClick={()=>void action('open')}><Icon name="folder" size={15}/>Open project…</button></div>{!!known.length&&<div className="dock-welcome-recent"><div className="dock-welcome-recent-heading"><h2>Recent</h2><button type="button" className="subtle-button" onClick={()=>void clearRecent()}>Clear all</button></div><ul>{known.slice(0,8).map(ws=><li key={ws.workspace_id}><button type="button" className="dock-welcome-open" onClick={()=>void openWorkspace(ws.workspace_id)}><Icon name="cube" size={16}/><span>{ws.name||'Untitled'}</span>{!!ws.running&&<small>Working</small>}</button><button type="button" className="icon-button" aria-label={`Remove ${ws.name||'Untitled'} from recent`} onClick={()=>void forgetRecent(ws.workspace_id)}><Icon name="close" size={14}/></button></li>)}</ul></div>}</div></div>}</div>
      {!!layout.documents.length&&groups(layout.right).some(g=>g.tabs.length)&&<><WidthDivider side="right" value={layout.rightWidth} onChange={width=>update(current=>({...current,rightWidth:width}))}/><div className="docked-sidebar right" style={{width:layout.rightWidth}}>{area('right')}</div></>}
    </div>
    {layout.documents.filter(doc=>{if(visible.has(doc.id))mounted.current.add(doc.id);return mounted.current.has(doc.id);}).map(doc=>{
      const sessionHost:SessionHost={workspaceId:doc.workspace,active:doc.id===layout.focus,visible:visible.has(doc.id),initialProject:projects.current.get(doc.workspace),header,rail,notices,tools:toolHosts,visibleTools,showTool,onOpen:openProject,onOpenWorkspace:workspace=>void openWorkspace(workspace),onProject:project=>{projects.current.set(project.workspace_id,project);update(current=>{const prior=current.documents.find(item=>item.id===doc.id);if(prior?.name===(project.name||'Untitled')&&prior.dirty===project.dirty)return current;return {...current,documents:current.documents.map(item=>item.workspace===project.workspace_id?{...item,name:project.name||'Untitled',dirty:project.dirty}:item)};});},connectMenu:channel=>connectMenu(doc.id,channel),connectView:channel=>connectView(doc.id,channel)};
      return <DockContent key={doc.id} host={host(doc.id)}><App host={sessionHost}/></DockContent>;
    })}
  </div></DockDragProvider>;
}
