import React,{createContext,useContext,useRef,useState,useEffect,type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import type {DockEdge} from './dock-layout';

export type DockDragPayload={kind:'tab'|'group';tabs:string[];active:string;area:string;group:string;label:string};
export type DockTarget={area:string;group:string;edge:DockEdge;before?:string;header?:boolean;stack?:boolean};
type DragState={payload:DockDragPayload;x:number;y:number;target:DockTarget|null};
type DragContext={drag:DragState|null;begin:(event:React.PointerEvent,payload:DockDragPayload)=>void};
const Context=createContext<DragContext|null>(null);
export const useDockDrag=()=>useContext(Context);
export const acceptsDockDrop=(source:string,target:string)=>(source===target&&(source==='canvas'||source==='chats'))||(['left','right'].includes(source)&&['left','right'].includes(target));
export function dockEdgeAt(x:number,y:number,rect:{left:number;top:number;width:number;height:number}):DockEdge{
  const dx=x-rect.left-rect.width/2,dy=y-rect.top-rect.height/2;
  // These regions match the five visible targets in the docking compass.
  if(Math.abs(dy)<=14&&Math.abs(dx)>14&&Math.abs(dx)<=44)return dx<0?'left':'right';
  if(Math.abs(dx)<=14&&Math.abs(dy)>14&&Math.abs(dy)<=44)return dy<0?'top':'bottom';
  const rx=(x-rect.left)/rect.width,ry=(y-rect.top)/rect.height;
  return rx<.22?'left':rx>.78?'right':ry<.22?'top':ry>.78?'bottom':'center';
}
export const dropLabel=(edge:DockEdge)=>({center:'Join tabs',left:'Split left',right:'Split right',top:'Split above',bottom:'Split below'})[edge];
function hitElement(x:number,y:number){
  const list=document.elementsFromPoint?.(x,y)||[];
  for(const node of list){
    if(!(node instanceof Element))continue;
    if(node.closest('.dock-drag-ghost,.dock-drop-preview,.dock-drop-compass'))continue;
    return node as HTMLElement;
  }
  return document.elementFromPoint?.(x,y) as HTMLElement|null;
}
function targetAt(x:number,y:number,payload:DockDragPayload):DockTarget|null{
  for(const node of document.querySelectorAll<HTMLElement>('[data-dock-outer]')){
    const rect=node.getBoundingClientRect(),area=node.dataset.dockOuter!;
    if(acceptsDockDrop(payload.area,area)&&x>=rect.left&&x<=rect.right&&y>=rect.top&&y<=rect.bottom)return {area,group:node.dataset.dockGroup!,edge:'center'};
  }
  const hit=hitElement(x,y);
  const stack=hit?.closest<HTMLElement>('[data-dock-stack]');
  if(stack&&acceptsDockDrop(payload.area,stack.dataset.dockArea!)){
    const heading=hit?.closest<HTMLElement>('.sidebar-section-heading');
    const section=hit?.closest<HTMLElement>('[data-sidebar-section]');
    const area=stack.dataset.dockArea!,group=stack.dataset.dockStack!;
    // Prefer split edges over the whole sidebar, including when the cursor is on a
    // section heading — headings sit in the top/side zones users aim for.
    const edge=dockEdgeAt(x,y,stack.getBoundingClientRect());
    if(edge!=='center')return {area,group,edge};
    let before=section?.dataset.sidebarSection;
    if(section){
      const rect=section.getBoundingClientRect();
      if(y>rect.top+rect.height/2)before=section.nextElementSibling?.getAttribute('data-sidebar-section')||undefined;
    }
    return {area,group,edge:'center',before,stack:true};
  }
  const pane=hit?.closest<HTMLElement>('[data-dock-group]'),area=pane?.closest<HTMLElement>('[data-dock-area]')?.dataset.dockArea;
  if(!pane||!area||!acceptsDockDrop(payload.area,area))return null;
  const group=pane.dataset.dockGroup!,header=hit?.closest<HTMLElement>('.dock-tabs');
  if(header){
    const tab=hit?.closest<HTMLElement>('.dock-tab');
    let before=tab?.querySelector<HTMLElement>('[data-tab]')?.dataset.tab;
    if(tab){const rect=tab.getBoundingClientRect();if(x>rect.left+rect.width/2)before=tab.nextElementSibling?.querySelector<HTMLElement>('[data-tab]')?.dataset.tab;}
    return {area,group,edge:'center',before,header:true};
  }
  const body=pane.querySelector<HTMLElement>('.dock-body');
  return body?{area,group,edge:dockEdgeAt(x,y,body.getBoundingClientRect())}:null;
}
export function DockDragProvider({children,onDrop}:{children:ReactNode;onDrop:(payload:DockDragPayload,target:DockTarget)=>void}){
  const [drag,setDrag]=useState<DragState|null>(null);
  const stop=useRef<(()=>void)|null>(null),drop=useRef(onDrop);drop.current=onDrop;
  useEffect(()=>()=>stop.current?.(),[]);
  function begin(event:React.PointerEvent,payload:DockDragPayload){
    if(event.button!==0||event.isPrimary===false||!payload.tabs.length)return;
    event.preventDefault();
    stop.current?.();
    const pointer=event.pointerId,source=event.currentTarget as HTMLElement,startX=event.clientX,startY=event.clientY;
    let started=false,last:DragState|null=null;
    const move=(next:PointerEvent)=>{
      if(next.pointerId!==pointer)return;
      if(!started&&Math.hypot(next.clientX-startX,next.clientY-startY)<6)return;
      if(!started){
        started=true;document.body.classList.add('dock-dragging');
        source.dataset.dockDragging='true';source.style.pointerEvents='none';
        try{source.setPointerCapture?.(pointer);}catch{}
      }
      next.preventDefault();
      last={payload,x:next.clientX,y:next.clientY,target:targetAt(next.clientX,next.clientY,payload)};
      setDrag(last);
    };
    const cleanup=()=>{
      window.removeEventListener('pointermove',move,true);window.removeEventListener('pointerup',finish,true);window.removeEventListener('pointercancel',cancel,true);window.removeEventListener('keydown',key,true);window.removeEventListener('blur',blur);
      document.body.classList.remove('dock-dragging');
      delete source.dataset.dockDragging;source.style.pointerEvents='';
      try{if(source.hasPointerCapture?.(pointer))source.releasePointerCapture(pointer);}catch{}
      setDrag(null);stop.current=null;
    };
    const cancel=(next?:PointerEvent)=>{if(next&&next.pointerId!==pointer)return;cleanup();};
    const finish=(next:PointerEvent)=>{
      if(next.pointerId!==pointer)return;
      const target=started?targetAt(next.clientX,next.clientY,payload):null;
      if(started){
        next.preventDefault();next.stopPropagation();
        // Prevent the compatibility click from selecting the tab at its old location.
        const suppress=(click:MouseEvent)=>{click.preventDefault();click.stopImmediatePropagation();};
        window.addEventListener('click',suppress,{capture:true,once:true});setTimeout(()=>window.removeEventListener('click',suppress,true),0);
      }
      cleanup();if(target)drop.current(payload,target);
    };
    const blur=()=>cancel();
    const key=(next:KeyboardEvent)=>{if(next.key==='Escape'){next.preventDefault();next.stopImmediatePropagation();cancel();}};
    window.addEventListener('pointermove',move,{capture:true,passive:false});window.addEventListener('pointerup',finish,true);window.addEventListener('pointercancel',cancel,true);window.addEventListener('keydown',key,true);window.addEventListener('blur',blur);
    stop.current=cleanup;
  }
  return <Context.Provider value={{drag,begin}}>{children}{drag&&createPortal(<div className="dock-drag-ghost" style={{left:Math.max(4,Math.min(drag.x+14,window.innerWidth-230)),top:Math.max(4,Math.min(drag.y+16,window.innerHeight-65))}}><strong>{drag.payload.label}</strong><span>{drag.target?drag.target.stack&&drag.target.edge==='center'?'Move section':dropLabel(drag.target.edge):drag.payload.area==='canvas'?'Drop in a canvas':'Drop in a sidebar'}</span></div>,document.body)}</Context.Provider>;
}
export function DockOuterTarget({area,group}:{area:'left'|'right';group:string}){
  const context=useDockDrag();if(!context?.drag||context.drag.payload.area==='canvas')return null;
  return <div className={`dock-outer-target ${area} ${context.drag.target?.area===area?'active':''}`} data-dock-outer={area} data-dock-group={group}><span>Dock {area}</span></div>;
}
