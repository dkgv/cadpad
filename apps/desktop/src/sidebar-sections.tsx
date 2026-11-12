import React,{useLayoutEffect,useRef,type CSSProperties} from 'react';
import {Icon} from './ui';
import {useDockDrag,dropLabel} from './dock-drag';
import type {DockEdge,DockGroup,DockTab} from './dock-layout';
function Content({host,open}:{host:HTMLElement;open:boolean}){
  const ref=useRef<HTMLDivElement>(null);
  useLayoutEffect(()=>{ref.current?.appendChild(host);return()=>host.remove();},[host]);
  return <div ref={ref} className="sidebar-section-content" hidden={!open}/>;
}
const MIN_SECTION=96;
export function SidebarSections({area,group,tabs,hosts,expanded,weights,onToggle,onDrop,onResize,sectionMode=false,loading={}}:{area:'left'|'right';group:DockGroup;tabs:Record<string,DockTab>;hosts:Record<string,HTMLElement>;expanded:string[];weights:Partial<Record<string,number>>;onToggle:(id:string)=>void;onDrop:(tab:string,source:string,before?:string)=>void;onResize:(next:Record<string,number>)=>void;sectionMode?:boolean;loading?:Record<string,boolean>}){
  const stack=useRef<HTMLDivElement>(null);
  const single=group.tabs.length===1&&!sectionMode;
  const active=group.active&&tabs[group.active]?group.active:group.tabs[0];
  const viewTitle=group.tabs.includes('move')&&group.tabs.includes('simulate')?'Motion':tabs[active]?.title||'Sidebar';
  const contextDrag=useDockDrag(),drag=sectionMode?null:contextDrag,hover=drag?.drag?.target?.area===area&&drag.drag.target.group===group.id?drag.drag.target:null;
  const before=hover?.edge==='center'?hover.before:null;
  const split=hover&&hover.edge!=='center'?hover:null;
  const open=group.tabs.filter(id=>tabs[id]&&(single||expanded.includes(id)));
  const weight= (id:string)=>Math.max(.2,weights[id]??1);
  function beginResize(event:React.PointerEvent<HTMLDivElement>,above:string,below:string){
    if(event.button!==0)return;
    event.preventDefault();event.stopPropagation();
    const root=stack.current;if(!root)return;
    const aboveNode=root.querySelector<HTMLElement>(`[data-sidebar-section="${above}"]`);
    const belowNode=root.querySelector<HTMLElement>(`[data-sidebar-section="${below}"]`);
    if(!aboveNode||!belowNode)return;
    const startY=event.clientY,aboveHeight=aboveNode.getBoundingClientRect().height,belowHeight=belowNode.getBoundingClientRect().height,total=aboveHeight+belowHeight;
    const pointer=event.pointerId,handle=event.currentTarget;
    try{handle.setPointerCapture?.(pointer);}catch{}
    const apply=(clientY:number)=>{
      const nextAbove=Math.max(MIN_SECTION,Math.min(total-MIN_SECTION,aboveHeight+(clientY-startY)));
      const nextBelow=total-nextAbove;
      // Keep the shared pair's relative mass so other open sections stay put.
      const pair=weight(above)+weight(below);
      onResize({[above]:pair*(nextAbove/total),[below]:pair*(nextBelow/total)});
    };
    const move=(next:PointerEvent)=>{if(next.pointerId!==pointer)return;next.preventDefault();apply(next.clientY);};
    const end=(next:PointerEvent)=>{
      if(next.pointerId!==pointer)return;
      window.removeEventListener('pointermove',move,true);window.removeEventListener('pointerup',end,true);window.removeEventListener('pointercancel',end,true);
      try{if(handle.hasPointerCapture?.(pointer))handle.releasePointerCapture?.(pointer);}catch{}
    };
    window.addEventListener('pointermove',move,{capture:true,passive:false});window.addEventListener('pointerup',end,true);window.addEventListener('pointercancel',end,true);
  }
  return <div ref={stack} className={`sidebar-sections ${sectionMode?'sidebar-content-sections':''} ${open.length>1?'is-stacked':''} ${hover?.edge==='center'&&!hover.before?'drop-at-end':''}`} data-dock-area={sectionMode?undefined:area} data-dock-stack={sectionMode?undefined:group.id} onDragOver={event=>{if(event.dataTransfer.types.includes('application/x-cadpad-tab'))event.preventDefault();}} onDrop={event=>{try{const data=JSON.parse(event.dataTransfer.getData('application/x-cadpad-tab'));event.preventDefault();event.stopPropagation();onDrop(data.tab,data.area,(event.target as Element).closest<HTMLElement>('[data-sidebar-section]')?.dataset.sidebarSection);}catch{}}}>
    {!sectionMode&&<header className="sidebar-view-heading"><button type="button" aria-label={`Move ${viewTitle} view`} title={viewTitle} onPointerDown={event=>{if(active)drag?.begin(event,{kind:single?'tab':'group',tabs:group.tabs,active,area,group:group.id,label:viewTitle});}}>{viewTitle}</button></header>}
    {group.tabs.map(id=>{
      if(!tabs[id])return null;
      const opened=single||expanded.includes(id);
      const prior=open.slice(0,open.indexOf(id)).at(-1);
      const share=opened&&open.length>1;
      const fill=opened&&open.length===1;
      return <React.Fragment key={id}>
        {opened&&prior&&<div role="separator" tabIndex={0} aria-orientation="horizontal" aria-label={`Resize ${tabs[prior].title} and ${tabs[id].title}`} aria-valuemin={MIN_SECTION} title="Drag to resize sections" className="sidebar-section-resizer" onPointerDown={event=>beginResize(event,prior,id)} onKeyDown={event=>{
          const step=event.shiftKey?36:12,direction=event.key==='ArrowDown'?1:event.key==='ArrowUp'?-1:0;
          if(!direction){if(event.key==='Enter'){event.preventDefault();onResize({[prior]:1,[id]:1});}return;}
          event.preventDefault();
          const pair=weight(prior)+weight(id),delta=(step/200)*direction;
          const nextAbove=Math.max(.2,Math.min(pair-.2,weight(prior)+delta));
          onResize({[prior]:nextAbove,[id]:pair-nextAbove});
        }}/>}
        <section className={`sidebar-section ${opened?'expanded':''} ${share?'stacked':''} ${fill?'fill':''} ${before===id?'drop-before':''}`} data-sidebar-section={id} style={share?{'--section-weight':weight(id)} as CSSProperties:undefined}>
          {!single&&<header className="sidebar-section-heading"><button aria-expanded={opened} aria-controls={`sidebar-content-${id}`} aria-label={`${opened?'Collapse':'Expand'} ${tabs[id].title} section`} onClick={()=>onToggle(id)} onPointerDown={event=>drag?.begin(event,{kind:'tab',tabs:[id],active:id,area,group:group.id,label:tabs[id].title})}><Icon name="chevron" size={11}/><span>{tabs[id].title}</span></button>{loading[id]&&<span className="sidebar-section-loading" role="status" aria-label={`Loading ${tabs[id].title}`}/>}</header>}
          <div id={`sidebar-content-${id}`} hidden={!opened}><Content host={hosts[id]} open={opened}/></div>
        </section>
      </React.Fragment>;
    })}
    {split&&<><div className={`dock-drop-preview ${split.edge}`}><span>{dropLabel(split.edge)}</span></div><div className="dock-drop-compass" aria-hidden="true">{(['top','left','center','right','bottom'] as DockEdge[]).map(edge=><span key={edge} className={`${edge} ${split.edge===edge?'active':''}`}>{edge==='center'?<Icon name="plus" size={13}/>:<Icon name={edge==='left'||edge==='right'?'splitRight':'splitDown'} size={13}/>}</span>)}</div></>}
  </div>;
}
