import type {Entity,SceneModel} from './index';

/** Exact revision-scoped IDs only; a tessellation ordinal is not an identity. */
export function sceneEntity(model:SceneModel,revision:string,entity:Entity|null):Entity|null {
  if(!entity||entity.revision!==revision)return null;
  const part=model.parts.find(part=>part.instance_id===entity.instance_id);
  return part&&(entity.entity_id==='part:0'||part.mesh.face_ids.includes(entity.entity_id)||part.mesh.edge_ids.includes(entity.entity_id))?entity:null;
}

export function sameEntity(a:Entity|null|undefined,b:Entity|null|undefined){return !!a&&!!b&&a.revision===b.revision&&a.instance_id===b.instance_id&&a.entity_id===b.entity_id;}
/** A, B, … Z, AA, AB … for referenced entities in selection order. */
export function referenceLetter(index:number){let text='';let n=index;do{text=String.fromCharCode(65+n%26)+text;n=Math.floor(n/26)-1;}while(n>=0);return text;}
