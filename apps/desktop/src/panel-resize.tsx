import React,{useEffect,useRef,useState} from 'react';

type PanelSizes={model:number;assistant:number;motion:number};
const defaults:PanelSizes={model:320,assistant:320,motion:320};
const storageKey='cadpad.panel-sizes.v2';
export function usePanelSizes(){
  const [sizes,setSizes]=useState<PanelSizes>(()=>{
    try{
      const current=localStorage.getItem(storageKey);
      const saved=JSON.parse(current||localStorage.getItem('cadpad.panel-sizes')||'null');
      if(!current&&saved){if(saved.model===230)saved.model=320;if(saved.assistant===360)saved.assistant=320;}
      return Object.fromEntries(Object.entries(defaults).map(([key,value])=>[key,Number.isFinite(saved?.[key])&&saved[key]>0?saved[key]:value])) as PanelSizes;
    }catch{return defaults;}
  });
  useEffect(()=>{try{localStorage.setItem(storageKey,JSON.stringify(sizes));}catch{}},[sizes]);
  return [sizes,(panel:keyof PanelSizes,value:number)=>setSizes(current=>({...current,[panel]:value}))] as const;
}

export function PanelResizeHandle({panel,value,onChange}:{panel:keyof PanelSizes;value:number;onChange:(value:number)=>void}){
  const reverse=panel==='assistant';
  const drag=useRef<{pointer:number;start:number;size:number}|null>(null);
  const min=260;
  const limit=(element:HTMLElement)=>{
    const bounds=element.parentElement?.parentElement?.getBoundingClientRect();
    return Math.max(min,Math.min(640,(bounds?.width||1000)-240));
  };
  const resize=(element:HTMLElement,size:number)=>onChange(Math.round(Math.max(min,Math.min(limit(element),size))));
  const end=(element:HTMLElement)=>{
    const pointer=drag.current?.pointer;drag.current=null;
    if(pointer!==undefined&&element.hasPointerCapture?.(pointer))element.releasePointerCapture(pointer);
  };
  return <div role="separator" tabIndex={0} aria-label={`Resize ${panel} panel`} aria-controls={panel==='assistant'?'panel-assistant-dock':`panel-${panel}`} aria-orientation="vertical" aria-valuemin={min} aria-valuenow={value} title="Drag to resize · double-click to reset" className={`panel-resizer vertical ${panel}`} onDoubleClick={()=>onChange(defaults[panel])} onPointerDown={event=>{
    if(event.button!==0)return;
    event.preventDefault();
    const bounds=event.currentTarget.parentElement!.getBoundingClientRect();
    drag.current={pointer:event.pointerId,start:event.clientX,size:bounds.width||value};
    event.currentTarget.setPointerCapture(event.pointerId);
  }} onPointerMove={event=>{
    const current=drag.current;if(!current||current.pointer!==event.pointerId)return;
    event.preventDefault();resize(event.currentTarget,current.size+(event.clientX-current.start)*(reverse?-1:1));
  }} onPointerUp={event=>end(event.currentTarget)} onPointerCancel={event=>end(event.currentTarget)} onLostPointerCapture={()=>{drag.current=null;}} onKeyDown={event=>{
    const step=event.shiftKey?40:10;
    const direction=event.key==='ArrowRight'?1:event.key==='ArrowLeft'?-1:0;
    if(direction){event.preventDefault();resize(event.currentTarget,value+direction*step*(reverse?-1:1));}
    else if(event.key==='Home'){event.preventDefault();onChange(min);}
    else if(event.key==='End'){event.preventDefault();resize(event.currentTarget,limit(event.currentTarget));}
    else if(event.key==='Enter'){event.preventDefault();onChange(defaults[panel]);}
  }}/>;
}
