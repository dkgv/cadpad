import React,{useEffect,useLayoutEffect,useRef,type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {Icon} from './ui';
import {useDockDrag,dockEdgeAt,dropLabel,acceptsDockDrop} from './dock-drag';

export type DockGroup={kind:'group';id:string;tabs:string[];active:string|null};
export type DockSplit={kind:'split';id:string;axis:'horizontal'|'vertical';ratio:number;first:DockNode;second:DockNode};
export type DockNode=DockGroup|DockSplit;
export type DockEdge='center'|'left'|'right'|'top'|'bottom';
export const dockId=()=>globalThis.crypto?.randomUUID?.()||`dock-${Date.now()}-${Math.random().toString(36).slice(2)}`;
export const group=(tabs:string[]=[],active:string|null=tabs[0]??null):DockGroup=>({kind:'group',id:dockId(),tabs,active});
export const groups=(node:DockNode):DockGroup[]=>node.kind==='group'?[node]:[...groups(node.first),...groups(node.second)];
export function updateGroup(node:DockNode,id:string,fn:(group:DockGroup)=>DockNode):DockNode{
  if(node.kind==='group')return node.id===id?fn(node):node;
  return {...node,first:updateGroup(node.first,id,fn),second:updateGroup(node.second,id,fn)};
}
export function removeTab(node:DockNode,tab:string):DockNode{
  if(node.kind==='group'){
    const index=node.tabs.indexOf(tab),tabs=node.tabs.filter(id=>id!==tab);
    return {...node,tabs,active:node.active===tab?tabs[Math.min(index,tabs.length-1)]??null:node.active};
  }
  const first=removeTab(node.first,tab),second=removeTab(node.second,tab);
  if(!groups(first).some(g=>g.tabs.length))return second;
  if(!groups(second).some(g=>g.tabs.length))return first;
  return {...node,first,second};
}
export function placeTab(node:DockNode,tab:string,target:string,edge:DockEdge='center',before?:string):DockNode{
  return placeTabs(node,[tab],target,edge,before,tab);
}
export function placeTabs(node:DockNode,incoming:string[],target:string,edge:DockEdge='center',before?:string,active=incoming[0]):DockNode{
  const original=groups(node).find(g=>g.id===target);if(!original||!incoming.length)return node;
  const moving=new Set(incoming);
  if(edge==='center'&&before&&moving.has(before)&&incoming.every(id=>original.tabs.includes(id)))return updateGroup(node,target,g=>({...g,active}));
  if(edge!=='center'&&original.tabs.length===incoming.length&&original.tabs.every(id=>moving.has(id)))return node;
  let destination=target;
  const result=updateGroup(node,target,current=>{
    const tabs=current.tabs.filter(id=>!moving.has(id));
    if(edge==='center'){
      const index=before?tabs.indexOf(before):-1;tabs.splice(index<0?tabs.length:index,0,...incoming);
      return {...current,tabs,active};
    }
    const fresh=group(incoming,active);destination=fresh.id;
    const prior={...current,tabs,active:moving.has(current.active??'')?tabs[0]??null:current.active};
    return {kind:'split',id:dockId(),axis:edge==='left'||edge==='right'?'horizontal':'vertical',ratio:.5,first:edge==='left'||edge==='top'?fresh:prior,second:edge==='left'||edge==='top'?prior:fresh};
  });
  function prune(current:DockNode):DockNode{
    if(current.kind==='group'){
      if(current.id===destination)return current;
      const tabs=current.tabs.filter(id=>!moving.has(id));
      return {...current,tabs,active:moving.has(current.active??'')?tabs[0]??null:current.active};
    }
    const first=prune(current.first),second=prune(current.second);
    if(!groups(first).some(g=>g.tabs.length))return second;
    if(!groups(second).some(g=>g.tabs.length))return first;
    return {...current,first,second};
  }
  return prune(result);
}
export function closeEmptyGroup(node:DockNode,id:string):DockNode|null{
  if(node.kind==='group')return node.id===id&&!node.tabs.length?null:node;
  const first=closeEmptyGroup(node.first,id),second=closeEmptyGroup(node.second,id);
  return !first?second:!second?first:{...node,first,second};
}
export function resizeSplit(node:DockNode,id:string,ratio:number):DockNode{
  if(node.kind==='group')return node;
  return node.id===id?{...node,ratio:Math.max(.15,Math.min(.85,ratio))}:{...node,first:resizeSplit(node.first,id,ratio),second:resizeSplit(node.second,id,ratio)};
}
export function validateDock(value:unknown,allowed:Set<string>,depth=0,seen=new Set<string>()):DockNode|null{
  if(!value||typeof value!=='object'||depth>64)return null;
  const node=value as DockNode;
  if(typeof node.id!=='string')return null;
  if(node.kind==='group'&&Array.isArray(node.tabs)){
    const tabs=node.tabs.filter(tab=>typeof tab==='string'&&allowed.has(tab)&&!seen.has(tab)&&!!seen.add(tab));
    return {...node,tabs,active:tabs.includes(node.active??'')?node.active:tabs[0]??null};
  }
  if(node.kind==='split'&&['horizontal','vertical'].includes(node.axis)){
    const first=validateDock(node.first,allowed,depth+1,seen),second=validateDock(node.second,allowed,depth+1,seen);
    if(!first)return second;if(!second)return first;
    return {...node,ratio:Number.isFinite(node.ratio)?Math.max(.15,Math.min(.85,node.ratio)):.5,first,second};
  }
  return null;
}

const dragType='application/x-cadpad-tab';
export type DockTab={id:string;title:string;dirty?:boolean;icon?:'cube'|'folder'|'motion'|'physics'|'tests'|'sparkles'|'chat';status?:'pending'|'unseen'};
function TabContextMenu({x,y,others,onCloseTab,onCloseOthers,onCloseAll,onDismiss}:{x:number;y:number;others:boolean;onCloseTab:()=>void;onCloseOthers:()=>void;onCloseAll:()=>void;onDismiss:()=>void}){
  const ref=useRef<HTMLDivElement>(null);
  const mac=/Mac|iPhone|iPad/.test(navigator.platform);
  useLayoutEffect(()=>{
    const node=ref.current;if(!node)return;
    const rect=node.getBoundingClientRect();
    node.style.left=`${Math.max(8,Math.min(x,window.innerWidth-rect.width-8))}px`;
    node.style.top=`${Math.max(8,Math.min(y,window.innerHeight-rect.height-8))}px`;
    node.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
  },[x,y]);
  useEffect(()=>{
    const down=(event:PointerEvent)=>{
      if(ref.current?.contains(event.target as Node))return;
      if(event.button===2&&(event.target as Element|null)?.closest?.('.dock-tab'))return;
      onDismiss();
    };
    const key=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();onDismiss();}};
    document.addEventListener('pointerdown',down,true);
    document.addEventListener('keydown',key);
    window.addEventListener('resize',onDismiss);
    return()=>{document.removeEventListener('pointerdown',down,true);document.removeEventListener('keydown',key);window.removeEventListener('resize',onDismiss);};
  },[onDismiss]);
  function keys(event:React.KeyboardEvent){
    const items=[...event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
    const index=items.indexOf(document.activeElement as HTMLButtonElement);
    const move=event.key==='ArrowDown'?1:event.key==='ArrowUp'?-1:0;
    if(move){event.preventDefault();items[(index+move+items.length)%items.length]?.focus();return;}
    if(event.key==='Home'||event.key==='End'){event.preventDefault();items[event.key==='Home'?0:items.length-1]?.focus();}
  }
  return <div ref={ref} className="tab-menu" role="menu" aria-label="Tab" style={{left:x,top:y}} onKeyDown={keys} onContextMenu={event=>{event.preventDefault();event.stopPropagation();}}>
    <button type="button" role="menuitem" aria-keyshortcuts={mac?'Meta+W':'Control+W'} onClick={onCloseTab}>Close<span className="tab-menu-shortcut">{mac?'⌘W':'Ctrl+W'}</span></button>
    <button type="button" role="menuitem" disabled={!others} onClick={onCloseOthers}>Close others</button>
    <button type="button" role="menuitem" onClick={onCloseAll}>Close all</button>
  </div>;
}
function Mount({host,visible}:{host:HTMLElement;visible:boolean}){
  const ref=useRef<HTMLDivElement>(null);
  useLayoutEffect(()=>{ref.current?.appendChild(host);return()=>{host.remove();};},[host]);
  return <div ref={ref} className="dock-mount" hidden={!visible}/>;
}
export function DockArea({tree,tabs,hosts,area,focused,onFocus,onChange,onDrop,onClose,extra}:{tree:DockNode;tabs:Record<string,DockTab>;hosts:Record<string,HTMLElement>;area:string;focused?:string;onFocus:(group:string,tab:string)=>void;onChange:(tree:DockNode)=>void;onDrop:(tab:string,source:string,group:string,edge:DockEdge,before?:string)=>void;onClose:(tabs:string[])=>void;extra?:(group:DockGroup)=>ReactNode}){
  const pointerDrag=useDockDrag();
  const [drop,setDrop]=React.useState<{group:string;edge:DockEdge}|null>(null);
  const [menu,setMenu]=React.useState<{tab:string;x:number;y:number}|null>(null);
  const dismissMenu=React.useCallback(()=>setMenu(null),[]);
  function showMenu(group:string,tab:string,x:number,y:number){
    onFocus(group,tab);
    window.dispatchEvent(new Event('cadpad-dismiss-tab-menu'));
    setMenu({tab,x,y});
  }
  const start=useRef<{id:string;axis:string;bounds:DOMRect}|null>(null);
  const rootRef=useRef<HTMLDivElement>(null);
  function decode(event:React.DragEvent){try{return JSON.parse(event.dataTransfer.getData(dragType)) as {tab:string;area:string};}catch{return null;}}
  function render(node:DockNode):ReactNode{
    if(node.kind==='split')return <div key={node.id} className={`dock-split ${node.axis}`} style={{'--dock-ratio':`${node.ratio*100}%`} as React.CSSProperties}>
      <div className="dock-branch">{render(node.first)}</div><div className="dock-divider" role="separator" tabIndex={0} aria-label={`Resize ${area} split`} aria-orientation={node.axis==='horizontal'?'vertical':'horizontal'} aria-valuemin={15} aria-valuemax={85} aria-valuenow={Math.round(node.ratio*100)} onDoubleClick={()=>onChange(resizeSplit(tree,node.id,.5))} onPointerDown={event=>{if(event.button!==0)return;event.preventDefault();start.current={id:node.id,axis:node.axis,bounds:event.currentTarget.parentElement!.getBoundingClientRect()};event.currentTarget.setPointerCapture(event.pointerId);}} onPointerMove={event=>{const drag=start.current;if(!drag||drag.id!==node.id)return;const ratio=drag.axis==='horizontal'?(event.clientX-drag.bounds.left)/drag.bounds.width:(event.clientY-drag.bounds.top)/drag.bounds.height;onChange(resizeSplit(tree,node.id,ratio));}} onPointerUp={()=>{start.current=null;}} onPointerCancel={()=>{start.current=null;}} onLostPointerCapture={()=>{start.current=null;}} onKeyDown={event=>{if(event.key==='Enter'){onChange(resizeSplit(tree,node.id,.5));return;}const delta=['ArrowRight','ArrowDown'].includes(event.key)?0.05:['ArrowLeft','ArrowUp'].includes(event.key)?-.05:0;if(delta){event.preventDefault();onChange(resizeSplit(tree,node.id,node.ratio+delta));}}}/><div className="dock-branch">{render(node.second)}</div>
    </div>;
    const hover=pointerDrag?.drag?.target?.area===area&&pointerDrag.drag.target.group===node.id?pointerDrag.drag.target:!pointerDrag?.drag&&drop?.group===node.id?drop:null;
    return <section key={node.id} className={`dock-group ${focused===node.id?'focused':''}`} data-dock-group={node.id}>
      <header className="dock-tabs" onPointerDown={event=>{if((event.target as Element).closest('.dock-tab,.dock-tab-actions,button,summary'))return;pointerDrag?.begin(event,{kind:'group',tabs:node.tabs,active:node.active??node.tabs[0],area,group:node.id,label:node.tabs.length>1?`${tabs[node.active??'']?.title||'Pane'} + ${node.tabs.length-1} tab${node.tabs.length===2?'':'s'}`:tabs[node.active??'']?.title||'Pane'});}}><div role="tablist" aria-label={`${area} tabs`} className="dock-tab-list" onDragOver={event=>{if(event.dataTransfer.types.includes(dragType))event.preventDefault();}} onDrop={event=>{const data=decode(event);if(!data)return;event.preventDefault();onDrop(data.tab,data.area,node.id,'center');setDrop(null);}}>
        {node.tabs.map(id=>tabs[id]&&<div className={`dock-tab ${node.active===id?'active':''} ${hover&&'before' in hover&&hover.before===id?'drop-before':''}`} key={id} onContextMenu={event=>{event.preventDefault();event.stopPropagation();showMenu(node.id,id,event.clientX,event.clientY);}} onPointerDown={event=>{if(!(event.target as Element).closest('.tab-close'))pointerDrag?.begin(event,{kind:'tab',tabs:[id],active:id,area,group:node.id,label:tabs[id].title});}} draggable={!pointerDrag} onDragStart={event=>{if(pointerDrag){event.preventDefault();return;}event.dataTransfer.setData(dragType,JSON.stringify({tab:id,area}));event.dataTransfer.effectAllowed='move';}} onDragEnd={()=>setDrop(null)} onDragOver={event=>event.preventDefault()} onDrop={event=>{const data=decode(event);if(!data)return;event.preventDefault();event.stopPropagation();onDrop(data.tab,data.area,node.id,'center',id);setDrop(null);}}>
          <button role="tab" aria-selected={node.active===id} tabIndex={node.active===id?0:-1} title={tabs[id].status==='pending'?`${tabs[id].title} — assistant working`:tabs[id].status==='unseen'?`${tabs[id].title} — unread reply`:tabs[id].title} aria-label={tabs[id].status==='pending'?`${tabs[id].title}, assistant working`:tabs[id].status==='unseen'?`${tabs[id].title}, unread reply`:undefined} onClick={()=>onFocus(node.id,id)} onKeyDown={event=>{const offset=event.key==='ArrowRight'?1:event.key==='ArrowLeft'?-1:0;if(offset){event.preventDefault();const next=node.tabs[(node.tabs.indexOf(id)+offset+node.tabs.length)%node.tabs.length];onFocus(node.id,next);(event.currentTarget.parentElement?.parentElement?.querySelector(`[data-tab="${next}"]`) as HTMLElement|null)?.focus();}}} data-tab={id}>{tabs[id].status==='pending'?<svg className="tab-spinner" viewBox="0 0 24 24" role="img" aria-hidden="true"><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" strokeWidth="2" opacity=".28"/><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeDasharray="13 38"/></svg>:tabs[id].status==='unseen'?<svg className="tab-unseen" viewBox="0 0 24 24" role="img" aria-hidden="true"><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" strokeWidth="1.8"/></svg>:tabs[id].icon&&<Icon name={tabs[id].icon!}/>}<span>{tabs[id].title}</span>{tabs[id].dirty&&<i className="tab-dirty" aria-label="Unsaved changes"/>}</button><button className="tab-close" aria-label={`Close ${tabs[id].title} tab`} title="Close tab" onClick={()=>onClose([id])}><Icon name="close" size={12}/></button>
        </div>)}
      </div><div className="dock-tab-actions">{!node.tabs.length&&<button className="icon-button" aria-label={`Close empty ${area} pane`} title="Close empty pane" onClick={()=>onChange(closeEmptyGroup(tree,node.id)??group())}><Icon name="close" size={13}/></button>}{extra?.(node)}</div></header>
      <div className="dock-body" onDragOver={event=>{if(!event.dataTransfer.types.includes(dragType))return;event.preventDefault();const rect=event.currentTarget.getBoundingClientRect(),x=(event.clientX-rect.left)/rect.width,y=(event.clientY-rect.top)/rect.height;const edge:DockEdge=x<.22?'left':x>.78?'right':y<.22?'top':y>.78?'bottom':'center';setDrop({group:node.id,edge});}} onDragLeave={event=>{if(!event.currentTarget.contains(event.relatedTarget as Node))setDrop(null);}} onDrop={event=>{const data=decode(event);if(!data)return;event.preventDefault();onDrop(data.tab,data.area,node.id,drop?.group===node.id?drop.edge:'center');setDrop(null);}}>
        {node.tabs.map(id=>hosts[id]&&<Mount key={id} host={hosts[id]} visible={node.active===id}/>)}
        {!node.tabs.length&&<div className="dock-empty">{hover?'Drop a tab here':area==='canvas'?'No model open':area==='chats'?'No conversation open':null}</div>}
        {hover&&!("header" in hover&&hover.header)&&<><div className={`dock-drop-preview ${hover.edge}`}><span>{dropLabel(hover.edge)}</span></div><div className="dock-drop-compass" aria-hidden="true">{(['top','left','center','right','bottom'] as DockEdge[]).map(edge=><span key={edge} className={`${edge} ${hover.edge===edge?'active':''}`}>{edge==='center'?<Icon name="plus" size={13}/>:<Icon name={edge==='left'||edge==='right'?'splitRight':'splitDown'} size={13}/>}</span>)}</div></>}
      </div>
    </section>;
  }
  // Panels render into stable portal hosts. Native capture listeners follow
  // their actual DOM placement when they move between groups.
  useLayoutEffect(()=>{
    const root=rootRef.current;if(!root)return;
    const owner=(target:EventTarget|null)=>{
      const element=(target as Element|null)?.closest?.('[data-dock-group]');
      return groups(tree).find(group=>group.id===element?.getAttribute('data-dock-group'));
    };
    const focus=(event:Event)=>{
      const current=owner(event.target);if(!current)return;
      const tab=(event.target as Element).closest('[data-tab]')?.getAttribute('data-tab')||current.active;
      if(tab)onFocus(current.id,tab);
    };
    const drag=(event:DragEvent)=>{
      if(!event.dataTransfer?.types.includes(dragType))return;
      const body=(event.target as Element)?.closest('.dock-body'),current=owner(event.target);
      if(!body||!current)return;
      try{const source=JSON.parse(event.dataTransfer.getData(dragType))?.area;if(source&&!acceptsDockDrop(source,area))return;}catch{}
      event.preventDefault();event.stopPropagation();
      const edge=dockEdgeAt(event.clientX,event.clientY,body.getBoundingClientRect());
      if(event.type==='drop'){
        try{const data=JSON.parse(event.dataTransfer.getData(dragType)) as {tab:string;area:string};onDrop(data.tab,data.area,current.id,edge);}catch{}
        setDrop(null);
      }else setDrop({group:current.id,edge});
    };
    const leave=(event:DragEvent)=>{if(!root.contains(event.relatedTarget as Node|null))setDrop(null);};
    root.addEventListener('dragleave',leave,true);
    root.addEventListener('pointerdown',focus,true);root.addEventListener('focusin',focus,true);
    root.addEventListener('dragover',drag,true);root.addEventListener('drop',drag,true);
    return()=>{root.removeEventListener('dragleave',leave,true);root.removeEventListener('pointerdown',focus,true);root.removeEventListener('focusin',focus,true);root.removeEventListener('dragover',drag,true);root.removeEventListener('drop',drag,true);};
  },[tree,onFocus,onDrop]);
  useEffect(()=>{const clear=()=>setDrop(null);window.addEventListener('dragend',clear);return()=>window.removeEventListener('dragend',clear);},[]);
  useEffect(()=>{const clear=()=>setMenu(null);window.addEventListener('cadpad-dismiss-tab-menu',clear);return()=>window.removeEventListener('cadpad-dismiss-tab-menu',clear);},[]);
  useEffect(()=>{if(menu&&!groups(tree).some(group=>group.tabs.includes(menu.tab)))setMenu(null);},[tree,menu]);
  const menuTabs=menu?(groups(tree).find(group=>group.tabs.includes(menu.tab))?.tabs.filter(id=>tabs[id])??[]):[];
  function closeTabs(ids:string[]){
    const current=menu?.tab;
    setMenu(null);
    if(ids.length)onClose(ids);
    if(current&&!ids.includes(current))requestAnimationFrame(()=>[...document.querySelectorAll<HTMLElement>('[data-tab]')].find(tab=>tab.dataset.tab===current)?.focus());
  }
  return <div ref={rootRef} className={`dock-area dock-${area}`} data-dock-area={area}>{render(tree)}{menu&&!!menuTabs.length&&createPortal(<TabContextMenu x={menu.x} y={menu.y} others={menuTabs.length>1} onCloseTab={()=>closeTabs([menu.tab])} onCloseOthers={()=>closeTabs(menuTabs.filter(id=>id!==menu.tab))} onCloseAll={()=>closeTabs(menuTabs)} onDismiss={dismissMenu}/>,document.body)}</div>;
}
export function DockContent({host,children}:{host:HTMLElement;children:ReactNode}){return createPortal(children,host);}
