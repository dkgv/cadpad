import * as THREE from 'three';

type Attribute=THREE.BufferAttribute|THREE.InterleavedBufferAttribute;
type Storage=THREE.BufferAttribute|THREE.InterleavedBuffer;
interface Resource {storage:Storage;bytes:number;owners:number;liveOwners:number;attributes:Map<string,Attribute>}
interface Lease {geometry:THREE.BufferGeometry;resources:Set<Resource>;active:boolean;retired:boolean;original:()=>void;bindings:Map<string,Resource>;disposeNow:()=>void;restore:()=>void}
interface Handoff {byArray:WeakMap<ArrayBufferLike,Map<string,Resource>>;resources:Set<Resource>;leases:Set<Lease>;geometries:WeakMap<THREE.BufferGeometry,Lease>;swapping:boolean;retire:(lease:Lease)=>void;trim:()=>void;uploaded:()=>void}
const eligible=new Set(['position','normal','uv','instanceStart','instanceEnd','@index']);
const storageOf=(attribute:Attribute):Storage=>'isInterleavedBufferAttribute' in attribute&&attribute.isInterleavedBufferAttribute?attribute.data:attribute as THREE.BufferAttribute;

/** Scene-local immutable CAD attributes, installed BEFORE the first GPU upload.
 * Geometries/materials/picking IDs stay occurrence-local. Three normally deletes
 * every attribute on geometry.dispose(), so surviving owners must be protected.
 * As with Three itself, an explicitly disposed geometry can be rendered again.
 */
export function shareRenderBuffers(objects:THREE.Object3D[],contextEvents?:Pick<EventTarget,'addEventListener'|'removeEventListener'>,handoff?:Handoff){
  let byArray=handoff?.byArray??new WeakMap<ArrayBufferLike,Map<string,Resource>>();const resources=new Set<Resource>();
  const leases=new Map<THREE.BufferGeometry,Lease>();
  let closed=false,inputBytes=0;
  const geometryMarkers=new Map<THREE.BufferGeometry,Storage>(),markerUsers=new Map<Storage,Set<THREE.BufferGeometry>>();
  const allGeometries=new Set<THREE.BufferGeometry>(),unmanagedStorage=new Set<Storage>();
  for(const root of objects)root.traverse(object=>{
    const geometry=(object as THREE.Mesh).geometry,marker=geometry?.getAttribute('componentId');
    if(!geometry?.isBufferGeometry)return;allGeometries.add(geometry);if(!marker)return;
    const storage=storageOf(marker);if(!storage.array?.buffer||typeof storage.onUploadCallback!=='function')return;geometryMarkers.set(geometry,storage);
    let users=markerUsers.get(storage);if(!users){users=new Set();markerUsers.set(storage,users);}users.add(geometry);
  });
  for(const geometry of allGeometries){
    const marker=geometryMarkers.get(geometry);if(marker&&markerUsers.get(marker)?.size===1)continue;
    for(const attribute of Object.values(geometry.attributes))unmanagedStorage.add(storageOf(attribute));
    if(geometry.index)unmanagedStorage.add(geometry.index);
  }
  function retain(lease:Lease){
    if(closed||lease.active)return;
    lease.active=true;for(const resource of lease.resources){resource.owners++;resource.liveOwners++;}handoff?.uploaded();
  }
  function intern(name:string,attribute:Attribute):{attribute:Attribute;resource:Resource}|null{
    if(!eligible.has(name))return null;
    const storage=storageOf(attribute),array=storage.array;
    if(unmanagedStorage.has(storage)||storage.usage!==THREE.StaticDrawUsage||storage.version!==0||!array?.buffer)return null;
    const interleaved='isInterleavedBufferAttribute' in attribute&&attribute.isInterleavedBufferAttribute;
    const layout=interleaved?`interleaved:${attribute.data.stride}:${'meshPerAttribute' in storage?storage.meshPerAttribute:0}`:
      `${name}:${attribute.itemSize}:${attribute.normalized}:${'gpuType' in attribute?attribute.gpuType:0}:${'isFloat16BufferAttribute' in attribute?attribute.isFloat16BufferAttribute:false}:${'meshPerAttribute' in attribute?attribute.meshPerAttribute:0}`;
    const key=`${array.constructor.name}:${array.byteOffset}:${array.byteLength}:${storage.usage}:${layout}`;
    let views=byArray.get(array.buffer);if(!views){views=new Map();byArray.set(array.buffer,views);}
    let resource=views.get(key);
    if(resource&&(resource.storage.version!==0||resource.storage.usage!==THREE.StaticDrawUsage))resource=undefined;
    if(!resource){resource={storage,bytes:array.byteLength,owners:0,liveOwners:0,attributes:new Map()};views.set(key,resource);}
    resources.add(resource);handoff?.resources.add(resource);
    const slot=interleaved?`${attribute.offset}:${attribute.itemSize}:${attribute.normalized}`:'attribute';
    let canonical=resource.attributes.get(slot);
    if(!canonical){
      // Start/end views of an interleaved line must use the SAME data object.
      // The publisher constructs these in consistent order for every instance.
      if(interleaved&&attribute.data!==resource.storage){
        canonical=new THREE.InterleavedBufferAttribute(resource.storage as THREE.InterleavedBuffer,attribute.itemSize,attribute.offset,attribute.normalized);
      }else canonical=attribute;
      resource.attributes.set(slot,canonical);
    }
    return {attribute:canonical,resource};
  }
  const seen=new Set<THREE.Object3D>();
  for(const root of objects)root.traverse(object=>{
    if(seen.has(object))return;seen.add(object);
    const geometry=(object as THREE.Mesh).geometry;
    if(!geometry?.isBufferGeometry)return;
    const marker=geometryMarkers.get(geometry);
    // The pinned viewer supplies occurrence-private IDs on every CAD geometry.
    // Without that upload marker, leave the geometry under Three's ownership.
    if(!marker||markerUsers.get(marker)?.size!==1)return;
    let lease=leases.get(geometry);
    if(!lease){
      if(handoff?.geometries.has(geometry))throw new Error('Render geometry is already owned');
      lease={geometry,resources:new Set(),active:false,retired:false,original:geometry.dispose,bindings:new Map(),disposeNow:()=>{},restore:()=>{}};
      for(const [name,attribute]of Object.entries(geometry.attributes) as [string,Attribute][]){
        const shared=intern(name,attribute);if(!shared)continue;
        geometry.setAttribute(name,shared.attribute);lease.resources.add(shared.resource);lease.bindings.set(name,shared.resource);
      }
      if(geometry.index){const shared=intern('@index',geometry.index);if(shared){geometry.setIndex(shared.attribute as THREE.BufferAttribute);lease.resources.add(shared.resource);lease.bindings.set('@index',shared.resource);}}
      if(!lease.resources.size)return;
      for(const resource of lease.resources)inputBytes+=resource.bytes;
      leases.set(geometry,lease);
      handoff?.leases.add(lease);
      handoff?.geometries.set(geometry,lease);
      const current=lease;
      const uploaded=marker.onUploadCallback;
      marker.onUploadCallback=function(){retain(current);uploaded.call(this);};
      current.restore=()=>{geometry.dispose=current.original;marker.onUploadCallback=uploaded;};
      current.disposeNow=()=>{
        const hidden=new Map<string,Attribute>();let hiddenIndex:THREE.BufferAttribute|null=null;
        if(current.active){
          current.active=false;for(const resource of current.resources){resource.owners--;if(!current.retired)resource.liveOwners--;}
        }
        // Repeated disposal must also protect other live owners.
        for(const [name,resource]of current.bindings){
          if(!resource.owners)continue;
          if(name==='@index'){hiddenIndex=geometry.index;geometry.setIndex(null);}
          else if(geometry.attributes[name]){hidden.set(name,geometry.attributes[name]);geometry.deleteAttribute(name);}
        }
        try{current.original.call(geometry);}finally{
          for(const [name,attribute]of hidden)geometry.setAttribute(name,attribute);
          if(hiddenIndex)geometry.setIndex(hiddenIndex);
        }
      };
      geometry.dispose=function(){
        if(current.retired)return;
        if(closed&&handoff?.swapping&&current.active){handoff.retire(current);return;}
        current.disposeNow();
      };
    }
  });
  const contextLost=()=>{for(const lease of leases.values())lease.active=false;for(const resource of resources){resource.owners=0;resource.liveOwners=0;}};
  contextEvents?.addEventListener('webglcontextlost',contextLost);
  const uniqueBytes=[...resources].reduce((sum,resource)=>sum+resource.bytes,0);
  return {stats:{geometries:leases.size,resources:resources.size,inputBytes,uniqueBytes,savedBytes:inputBytes-uniqueBytes},
    dispose(){if(closed)return;closed=true;
      for(const lease of leases.values())lease.geometry.dispose();
      for(const lease of leases.values())if(!lease.retired){lease.restore();handoff?.leases.delete(lease);handoff?.geometries.delete(lease.geometry);}
      contextEvents?.removeEventListener('webglcontextlost',contextLost);
      leases.clear();resources.clear();seen.clear();geometryMarkers.clear();markerUsers.clear();allGeometries.clear();unmanagedStorage.clear();byArray=new WeakMap();
      handoff?.trim();
    }};
}

/** Carry registered GPU owners across a synchronous scene replacement. A new
 * geometry's upload callback establishes its disposal listener before ownership
 * transfers. Never-uploaded replacements keep a bounded old registered owner. */
export function revisionRenderBuffers(contextEvents?:Pick<EventTarget,'addEventListener'|'removeEventListener'>,maxRetainedBytes=64*1024*1024){
  if(!Number.isSafeInteger(maxRetainedBytes)||maxRetainedBytes<0)throw new Error('Invalid render retention budget');
  const keepers=new Set<Lease>();let closed=false;
  function size(lease:Lease){
    const stores=new Set(Object.values(lease.geometry.attributes).map(attribute=>storageOf(attribute as Attribute)));
    if(lease.geometry.index)stores.add(lease.geometry.index);
    return [...stores].reduce((sum,storage)=>sum+(storage.array?.byteLength??0),0);
  }
  function finish(lease:Lease){lease.disposeNow();lease.restore();keepers.delete(lease);bank.leases.delete(lease);bank.geometries.delete(lease.geometry);}
  const bank:Handoff={
    byArray:new WeakMap(),resources:new Set(),leases:new Set(),geometries:new WeakMap(),swapping:false,
    uploaded(){if(keepers.size)bank.trim();},
    retire(lease){if(lease.retired)return;lease.retired=true;for(const resource of lease.resources)resource.liveOwners--;keepers.add(lease);},
    trim(){
      if(closed||bank.swapping)return;
      const referenced=new Set([...bank.leases].filter(lease=>!lease.retired).flatMap(lease=>[...lease.resources]));
      const needed=new Set([...referenced].filter(resource=>resource.liveOwners===0&&resource.owners>0));
      let bytes=0,count=0;
      for(const lease of [...keepers].reverse()){
        const cost=size(lease);
        if(count<4096&&cost<=maxRetainedBytes-bytes&&[...lease.resources].some(resource=>needed.has(resource))){
          bytes+=cost;count++;for(const resource of lease.resources)needed.delete(resource);
        }else finish(lease);
      }
      const retained=new Set([...bank.leases].flatMap(lease=>[...lease.resources]));
      for(const resource of bank.resources)if(!retained.has(resource))bank.resources.delete(resource);
    },
  };
  const contextLost=()=>{
    for(const lease of bank.leases)lease.active=false;
    for(const resource of bank.resources){resource.owners=0;resource.liveOwners=0;}
    for(const lease of [...keepers])finish(lease);
    bank.trim();
  };
  contextEvents?.addEventListener('webglcontextlost',contextLost);
  return {
    get stats(){return {geometries:bank.leases.size,retainedGeometries:keepers.size,retainedBytes:[...keepers].reduce((sum,lease)=>sum+size(lease),0)};},
    attach(objects:THREE.Object3D[]){
      if(closed)throw new Error('Render storage is closed');
      const previous=new Set(bank.leases);
      try{const scene=shareRenderBuffers(objects,undefined,bank);bank.trim();return scene;}
      catch(error){for(const lease of [...bank.leases])if(!previous.has(lease))finish(lease);bank.swapping=false;bank.trim();throw error;}
    },
    beginSwap(){if(closed)throw new Error('Render storage is closed');bank.swapping=true;},
    finishSwap(){bank.swapping=false;bank.trim();},
    dispose(){if(closed)return;closed=true;bank.swapping=false;
      for(const lease of [...bank.leases])finish(lease);
      contextEvents?.removeEventListener('webglcontextlost',contextLost);
      keepers.clear();bank.resources.clear();bank.leases.clear();bank.byArray=new WeakMap();bank.geometries=new WeakMap();
    },
  };
}
