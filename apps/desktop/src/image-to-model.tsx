import React, {useState} from 'react';
import {Icon} from './ui';

export type ModelImage={data:string;mime:string;name:string};
/** One generated version; the label is shown on its button. */
export type ModelVersionChoice={label:string;title?:string};

/** Largest image the Cadpad cloud accepts for image→model generation. */
export const MAX_MODEL_IMAGE=15*1024*1024;
export function readModelImage(file:File):Promise<ModelImage>{
  if(!file.type.startsWith('image/'))return Promise.reject(new Error('Choose an image file (PNG, JPG, WEBP, GIF or HEIC).'));
  if(file.size>MAX_MODEL_IMAGE)return Promise.reject(new Error('Choose an image smaller than 15 MiB.'));
  return new Promise((resolve,reject)=>{
    const reader=new FileReader();
    reader.onload=()=>{const url=String(reader.result);resolve({data:url.slice(url.indexOf(',')+1),mime:file.type||'image/png',name:file.name||'image.png'});};
    reader.onerror=()=>reject(new Error('Could not read that image.'));
    reader.readAsDataURL(file);
  });
}

export function ImageToModel({signedIn,signingIn,image,guidance,generating,versions,staged,disabled,onSignIn,onChoose,onFile,onClear,onGuidance,onGenerate,onVersion,onDismissVersions}:{
  /** null while the keychain check is pending. */
  signedIn:boolean|null;signingIn:boolean;
  image:ModelImage|null;guidance:string;generating:boolean;
  versions:ModelVersionChoice[];staged:number;
  /** No open model, or another change or build is running. */
  disabled:boolean;
  onSignIn:()=>void;onChoose:()=>void;onFile:(file:File)=>void;onClear:()=>void;onGuidance:(value:string)=>void;onGenerate:()=>void;onVersion:(index:number)=>void;onDismissVersions:()=>void;
}){
  const [dropDepth,setDropDepth]=useState(0);
  const firstImage=(list:DataTransferItemList|FileList|null)=>[...(list??[])].map(item=>item instanceof File?item:item.kind==='file'?item.getAsFile():null).find((file):file is File=>!!file&&file.type.startsWith('image/'));
  const drag=(event:React.DragEvent,phase:'enter'|'over'|'leave'|'drop')=>{
    if(!event.dataTransfer.types.includes('Files'))return;
    event.preventDefault();
    if(phase==='over'){event.dataTransfer.dropEffect=generating?'none':'copy';return;}
    if(phase==='enter'){setDropDepth(depth=>depth+1);return;}
    if(phase==='leave'){setDropDepth(depth=>Math.max(0,depth-1));return;}
    setDropDepth(0);
    const file=firstImage(event.dataTransfer.files);
    if(file&&!generating)onFile(file);
  };
  return <aside className="image-model-panel inspector-page" id="panel-image" aria-label="Cadpad Cloud" tabIndex={-1}
    onPaste={event=>{if((event.target as HTMLElement).closest('textarea'))return;const file=firstImage(event.clipboardData.items);if(file&&!generating){event.preventDefault();onFile(file);}}}>
    <div className="image-model-content">
      <p className="muted">Turn a photo, sketch or screenshot into an editable model in the open document.</p>
      {signedIn===false&&<section className="image-model-signin">
        <strong>Sign in to Cadpad</strong>
        <p className="muted">Images are processed by Cadpad Cloud.</p>
        <button className="primary-button" disabled={signingIn} onClick={onSignIn}>{signingIn?'Waiting for browser…':'Sign in with browser'}</button>
      </section>}
      <div className={`image-model-drop${dropDepth?' is-dragging':''}${image?' has-image':''}`} onDragEnter={event=>drag(event,'enter')} onDragOver={event=>drag(event,'over')} onDragLeave={event=>drag(event,'leave')} onDrop={event=>drag(event,'drop')}>
        {image?<>
          <img src={`data:${image.mime};base64,${image.data}`} alt={`Selected image ${image.name}`}/>
          <div className="image-model-file"><span title={image.name}>{image.name}</span><button type="button" className="icon-button" aria-label="Remove image" disabled={generating} onClick={onClear}><Icon name="close" size={13}/></button></div>
        </>:<>
          <Icon name="image" size={26}/>
          <p>Drop an image here, or paste one.</p>
          <button type="button" onClick={onChoose}>Choose image…</button>
        </>}
      </div>
      {image&&!generating&&<button type="button" className="image-model-replace" onClick={onChoose}>Choose another image…</button>}
      <label className="image-model-guidance">
        <span>Guidance <small>optional</small></span>
        <textarea value={guidance} disabled={generating} placeholder="e.g. a wall bracket, 3 mm thick, two M4 holes" onChange={event=>onGuidance(event.target.value)}/>
      </label>
      <button type="button" className="primary-button image-model-generate" disabled={!image||generating||disabled||signedIn===false} onClick={onGenerate}>
        {generating?<><span className="cloud-spinner" aria-hidden="true"/>Generating…</>:<><Icon name="sparkles" size={14}/>Generate model</>}
      </button>
      {generating&&<p className="muted" role="status">This can take a minute or two. The result opens as a new revision of the current model.</p>}
      {!generating&&disabled&&image&&<p className="muted">Open a model and wait for any build to finish before generating.</p>}
      {!!versions.length&&<section className="image-model-versions" aria-label="Generated versions">
        <header><strong>Versions</strong><button type="button" className="icon-button" aria-label="Dismiss generated versions" onClick={onDismissVersions}><Icon name="close" size={13}/></button></header>
        <p className="muted">Pick the version closest to your image. Each choice is saved as a new revision.</p>
        <div className="image-model-version-list">{versions.map((version,index)=><button key={index} type="button" className={index===staged?'is-active':''} aria-pressed={index===staged} title={version.title} disabled={generating||disabled&&index!==staged} onClick={()=>onVersion(index)}>{version.label}</button>)}</div>
      </section>}
    </div>
  </aside>;
}
