export interface BufferDescriptor {handle:string;bytes:number;sha256:string}
/** One scoped scene load shares immutable bytes between identical occurrences.
 * The cache ends with the load; it cannot retain buffers from previous revisions. */
export function sceneBuffers(artifact:(handle:string)=>Promise<ArrayBuffer>){
  const pending=new Map<string,{descriptor:BufferDescriptor;data:Promise<ArrayBuffer>}>();
  return (descriptor:BufferDescriptor):Promise<ArrayBuffer>=>{
    if(!Number.isSafeInteger(descriptor.bytes)||descriptor.bytes<0||descriptor.bytes>1024**3||!/^([0-9a-f]{64})$/.test(descriptor.sha256))return Promise.reject(new Error('Invalid mesh buffer descriptor'));
    const existing=pending.get(descriptor.handle);
    if(existing){if(existing.descriptor.bytes!==descriptor.bytes||existing.descriptor.sha256!==descriptor.sha256)return Promise.reject(new Error('Conflicting mesh buffer descriptors'));return existing.data;}
    const data=artifact(descriptor.handle).then(data=>{if(data.byteLength!==descriptor.bytes||data.byteLength%4)throw new Error('Invalid mesh artifact');return data;});
    pending.set(descriptor.handle,{descriptor,data});return data;
  };
}

interface CachedBytes {data:ArrayBuffer;verified:boolean}
/** Retain only verified bytes used by the committed scene, within one workspace.
 * Active scenes/in-flight loads own their arrays separately from this byte cap.
 * No secure hashing API means ordinary loading, with cross-revision reuse off. */
export function revisionBuffers(maxBytes=64*1024*1024){
  if(!Number.isSafeInteger(maxBytes)||maxBytes<0)throw new Error('Invalid mesh cache budget');
  let resident=new Map<string,ArrayBuffer>(),scope:string|null=null,generation=0;
  async function matches(data:ArrayBuffer,sha256:string):Promise<boolean|null>{
    if(!globalThis.crypto?.subtle)return null;
    let digest:ArrayBuffer;
    try{digest=await globalThis.crypto.subtle.digest('SHA-256',data);}catch{return null;}
    return [...new Uint8Array(digest)].map(byte=>byte.toString(16).padStart(2,'0')).join('')===sha256;
  }
  return {
    get stats(){return {entries:resident.size,bytes:[...resident.values()].reduce((sum,data)=>sum+data.byteLength,0)};},
    clear(){generation++;resident.clear();scope=null;},
    begin(workspace:string|null,artifact:(handle:string)=>Promise<ArrayBuffer>){
      const token=++generation,previous=workspace&&workspace===scope?resident:new Map<string,ArrayBuffer>();
      const handles=new Map<string,string>(),pending=new Map<string,Promise<ArrayBuffer>>(),ready=new Map<string,CachedBytes>();
      let outstanding=0,failed=false,closed=false,fetchedBytes=0,reusedBytes=0;
      return {
        get stats(){return {fetchedBytes,reusedBytes};},
        load(descriptor:BufferDescriptor):Promise<ArrayBuffer>{
          if(closed)return Promise.reject(new Error('Mesh load is closed'));
          if(!Number.isSafeInteger(descriptor.bytes)||descriptor.bytes<0||descriptor.bytes>1024**3||descriptor.bytes%4||!/^([0-9a-f]{64})$/.test(descriptor.sha256)){
            failed=true;return Promise.reject(new Error('Invalid mesh buffer descriptor'));
          }
          const key=`${descriptor.bytes}:${descriptor.sha256}`,existing=handles.get(descriptor.handle);
          if(existing&&existing!==key){failed=true;return Promise.reject(new Error('Conflicting mesh buffer descriptors'));}
          handles.set(descriptor.handle,key);
          const found=pending.get(key);if(found)return found;
          outstanding++;
          const data=Promise.resolve().then(async()=>{
            const cached=previous.get(key);
            // A consumer must not mutate immutable CAD data. Verify again so a
            // changed old view cannot silently become the next revision's mesh.
            if(cached&&await matches(cached,descriptor.sha256)===true){
              reusedBytes+=cached.byteLength;if(!closed)ready.set(key,{data:cached,verified:true});return cached;
            }
            const bytes=await artifact(descriptor.handle);
            if(bytes.byteLength!==descriptor.bytes||bytes.byteLength%4)throw new Error('Invalid mesh artifact');
            const verified=await matches(bytes,descriptor.sha256);
            if(verified===false)throw new Error('Mesh artifact checksum mismatch');
            fetchedBytes+=bytes.byteLength;if(!closed)ready.set(key,{data:bytes,verified:verified===true});return bytes;
          }).then(bytes=>{outstanding--;return bytes;},error=>{outstanding--;failed=true;throw error;});
          pending.set(key,data);return data;
        },
        commit(){
          if(closed||failed||outstanding||token!==generation)return false;
          closed=true;
          const next=new Map<string,ArrayBuffer>();let bytes=0;
          if(workspace)for(const [key,entry]of ready){
            if(!entry.verified||entry.data.byteLength>maxBytes-bytes||next.size>=4096)continue;
            next.set(key,entry.data);bytes+=entry.data.byteLength;
          }
          resident=next;scope=workspace;return true;
        },
        discard(){closed=true;pending.clear();ready.clear();handles.clear();},
      };
    },
  };
}
