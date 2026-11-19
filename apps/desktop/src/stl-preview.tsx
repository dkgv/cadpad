import {useEffect,useRef} from 'react';

export class StlPreviewStream {
  private chunks:ArrayBuffer[]=[];
  private listener?: (chunk:ArrayBuffer)=>void;

  push(chunk:ArrayBuffer){
    if(!(chunk instanceof ArrayBuffer)||!chunk.byteLength||chunk.byteLength%36)return;
    this.chunks.push(chunk);
    this.listener?.(chunk);
  }

  subscribe(listener:(chunk:ArrayBuffer)=>void){
    this.listener=listener;
    this.chunks.forEach(listener);
    return ()=>{this.listener=undefined;};
  }
}

/** The preview owns its own temporary canvas and never exposes partial topology. */
export function StlPreview({stream,onFrame}:{stream:StlPreviewStream;onFrame:()=>void}){
  const host=useRef<HTMLDivElement>(null);
  useEffect(()=>{
    const container=host.current!;
    let active=true;
    let release:(()=>void)|undefined;
    void import('@cadpad/viewer/stl-preview').then(({stlPreviewViewport})=>{
      if(!active)return;
      const viewport=stlPreviewViewport(container,onFrame);
      const unsubscribe=stream.subscribe(viewport.add);
      release=()=>{unsubscribe();viewport.dispose();};
    }).catch(()=>{/* The standard loading indicator stays visible. */});
    return ()=>{active=false;release?.();};
  },[stream,onFrame]);
  return <div ref={host} className="stl-preview" aria-hidden="true">
    <span>Loading full model…</span>
  </div>;
}
