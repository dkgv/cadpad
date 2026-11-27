import type {ReviewPart,ReviewSnapshot,SceneModel} from '@cadpad/viewer';

type Loader=(handle:string)=>Promise<ArrayBuffer>;
/** Display meshes of past revisions are collected, so the "before" side of a review is captured while it is still current. */
const MAX_SNAPSHOTS=8;
const snapshots=new Map<string,ReviewSnapshot>();
const key=(workspace:string,revision:string)=>`${workspace}:${revision}`;

export async function captureSurfaces(workspace:string,revision:string,model:SceneModel,load:Loader):Promise<ReviewSnapshot>{
  const parts=await Promise.all(model.parts.map(async(part):Promise<ReviewPart|null>=>{
    const {vertices,triangles}=part.mesh.buffers;
    if(!vertices||!triangles)return null;
    const [v,t]=await Promise.all([load(vertices.handle),load(triangles.handle)]);
    if(v.byteLength!==vertices.bytes||t.byteLength!==triangles.bytes||v.byteLength%12||t.byteLength%12)throw new Error('Invalid mesh artifact');
    return {instance_id:part.instance_id,name:part.name,position:part.position,volume:part.volume,vertices:new Float32Array(v),triangles:new Uint32Array(t)};
  }));
  return {workspace,revision,parts:parts.filter((part):part is ReviewPart=>!!part)};
}
export function rememberSnapshot(snapshot:ReviewSnapshot){
  const id=key(snapshot.workspace,snapshot.revision);
  snapshots.delete(id);snapshots.set(id,snapshot);
  while(snapshots.size>MAX_SNAPSHOTS)snapshots.delete(snapshots.keys().next().value!);
}
export function reviewSnapshot(workspace:string,revision:string){return snapshots.get(key(workspace,revision))??null;}

/** Signed volume change in model units, formatted for a cubic-millimetre model. */
export function volumeChange(before:number|null,after:number|null){
  if(before==null||after==null)return null;
  const delta=after-before;
  if(Math.abs(delta)<1e-6)return 'Volume unchanged';
  const sign=delta>0?'+':'−',size=Math.abs(delta);
  const text=size>=1000?`${(size/1000).toFixed(size>=100_000?0:1)} cm³`:`${size.toFixed(size>=10?0:1)} mm³`;
  return `Volume ${sign}${text}`;
}
