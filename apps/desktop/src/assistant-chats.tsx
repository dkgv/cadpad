import React,{useEffect,useMemo,useRef,useState} from 'react';
import {Icon} from './ui';
import {ChatHistory} from './chat-history';
import {DockArea,DockContent,group,groups,placeTabs,removeTab,updateGroup,type DockEdge,type DockNode,type DockTab} from './dock-layout';
import {DockDragProvider,type DockDragPayload,type DockTarget} from './dock-drag';

export type ChatSummary={id:string;title:string;created_at:number;updated_at:number;message_count:number};

export function chatTabIds(chats:ChatSummary[],activeId:string|null){
  const ids=chats.filter(chat=>chat.message_count>0).map(chat=>chat.id);
  if(activeId&&!ids.includes(activeId))ids.push(activeId);
  return ids.length?ids:[activeId||'new'];
}

export function syncChatDock(tree:DockNode,ids:string[],activeId?:string|null):DockNode{
  const allowed=new Set(ids);
  let next=tree;
  for(const id of groups(next).flatMap(pane=>pane.tabs))if(!allowed.has(id))next=removeTab(next,id);
  const panes=groups(next).filter(pane=>pane.tabs.length);
  if(!panes.length)return group(ids,activeId&&ids.includes(activeId)?activeId:ids[0]??null);
  const present=new Set(panes.flatMap(pane=>pane.tabs));
  for(const id of ids){
    if(present.has(id))continue;
    const target=panes.find(pane=>pane.tabs.includes(activeId||''))??panes[0];
    next=placeTabs(next,[id],target.id,'center',undefined,activeId===id?id:target.active??id);
  }
  if(activeId&&ids.includes(activeId)){
    const owner=groups(next).find(pane=>pane.tabs.includes(activeId));
    if(owner&&owner.active!==activeId)next=updateGroup(next,owner.id,pane=>({...pane,active:activeId}));
  }
  return next;
}

export function AssistantChatDock({
  workspaceKey,
  chats,
  activeId,
  busy,
  hosts,
  renderChat,
  onVisibleChats,
  onNew,
  onSelect,
  onClose,
  onRestore,
}:{
  workspaceKey:string;
  chats:ChatSummary[];
  activeId:string|null;
  busy:boolean;
  hosts:Record<string,HTMLElement>;
  renderChat:(chatId:string)=>React.ReactNode;
  onVisibleChats:(ids:string[])=>void;
  onNew:()=>Promise<boolean>;
  onSelect:(id:string)=>Promise<boolean>;
  onClose:(id:string)=>Promise<boolean>;
  onRestore:(id:string)=>Promise<boolean>;
}){
  const dock=useRef<HTMLDivElement>(null);
  const [emptyClosed,setEmptyClosed]=useState(false);
  const ids=useMemo(()=>chatTabIds(chats,activeId).filter(id=>!emptyClosed||chats.some(chat=>chat.id===id&&chat.message_count>0)),[chats,activeId,emptyClosed]);
  useEffect(()=>{dock.current?.dispatchEvent(new CustomEvent('cadpad-chat-tabs-change',{bubbles:true,detail:{count:ids.length}}));},[ids.length,workspaceKey]);
  const [tree,setTree]=useState<DockNode>(()=>group(ids,activeId&&ids.includes(activeId)?activeId:ids[0]??null));
  const [removed,setRemoved]=useState<string|null>(null);
  const [focusedGroup,setFocusedGroup]=useState<string|undefined>();
  useEffect(()=>{setEmptyClosed(false);setTree(current=>syncChatDock(current,ids,activeId));setRemoved(null);setFocusedGroup(undefined);},[workspaceKey]);
  useEffect(()=>{setTree(current=>syncChatDock(current,ids,activeId));},[ids.join('\0'),activeId]);
  const visibleIds=groups(tree).flatMap(pane=>pane.active&&ids.includes(pane.active)?[pane.active]:[]);
  const visibleKey=JSON.stringify(visibleIds);
  useEffect(()=>{onVisibleChats(visibleIds);},[visibleKey,onVisibleChats]);
  const tabs=useMemo(()=>{
    const titles=new Map(chats.map(chat=>[chat.id,chat.title]));
    return Object.fromEntries(ids.map(id=>[id,{id,title:titles.get(id)||(id===activeId?'New chat':'Chat'),icon:'chat' as const} satisfies DockTab]));
  },[ids,chats,activeId]);
  function drop(tab:string,_source:string,target:string,edge:DockEdge,before?:string,payload?:DockDragPayload){
    if(busy)return;
    setTree(current=>{
      const incoming=(payload?.tabs||[tab]).filter(id=>ids.includes(id));
      if(!incoming.length||!groups(current).some(pane=>pane.id===target))return current;
      const active=incoming.includes(tab)?tab:incoming[0];
      return placeTabs(current,incoming,target,edge,edge==='center'?before:undefined,active);
    });
  }
  async function focus(groupId:string,tab:string){
    if(tab!==activeId&&tab!=='new'&&!await onSelect(tab))return false;
    setFocusedGroup(groupId);
    setTree(current=>updateGroup(current,groupId,pane=>pane.active===tab?pane:{...pane,active:tab}));
    return true;
  }
  async function close(tab:string){
    if(busy||!ids.includes(tab))return;
    const summary=chats.find(chat=>chat.id===tab);
    if(summary&&summary.message_count>0){
      if(await onClose(tab)){setRemoved(tab);if(ids.length===1)setEmptyClosed(true);}
      return;
    }
    setEmptyClosed(true);
  }
  useEffect(()=>{
    const handler=(event:Event)=>{
      const id=(event as CustomEvent<{chatId:string}>).detail?.chatId;
      if(dock.current?.contains(event.target as Node)&&id&&visibleIds.includes(id))void close(id);
    };
    document.addEventListener('cadpad-close-chat',handler);
    return()=>document.removeEventListener('cadpad-close-chat',handler);
  });
  return <DockDragProvider onDrop={(payload:DockDragPayload,target:DockTarget)=>{if(payload.area==='chats'&&target.area==='chats')drop(payload.active,payload.area,target.group,target.edge,target.before,payload);}}>
    <div ref={dock} className="assistant-chat-dock">
      <DockArea
        area="chats"
        tree={tree}
        tabs={tabs}
        hosts={hosts}
        focused={focusedGroup??groups(tree).find(pane=>pane.tabs.includes(activeId||''))?.id}
        onFocus={(groupId,tab)=>{void focus(groupId,tab);}}
        onChange={setTree}
        onDrop={(tab,source,groupId,edge,before)=>drop(tab,source,groupId,edge,before)}
        onClose={id=>{void close(id);}}
        extra={()=><><button type="button" className="icon-button" disabled={busy} title="New chat" aria-label="New chat" onClick={()=>void onNew().then(created=>{if(created)setEmptyClosed(false);})}><Icon name="plus" size={14}/></button><ChatHistory key={workspaceKey} chats={chats} activeId={activeId} busy={busy} onSelect={async id=>{const owner=groups(tree).find(pane=>pane.tabs.includes(id));return owner?focus(owner.id,id):false;}}/></>}
      />
      <div className="chat-removal" hidden={!removed}>Chat removed. <button type="button" disabled={busy} onClick={async()=>{if(removed&&await onRestore(removed))setRemoved(null);}}>Undo</button></div>
      {busy&&<span className="visually-hidden">Wait for the assistant to finish or stop it before changing chats.</span>}
      {visibleIds.map(id=>hosts[id]&&<DockContent key={`${workspaceKey}:${id}`} host={hosts[id]}>{renderChat(id)}</DockContent>)}
    </div>
  </DockDragProvider>;
}
