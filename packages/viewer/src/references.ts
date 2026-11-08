import type {Anchor,DimensionDefinition,Entity,GeometryRef} from './index';
export interface Inspection {
  revision:string;
  pose_version:number;
  entities?:{reference:Entity;anchor?:Extract<Anchor,{kind:'geometry_center'}>;circular_definition?:Extract<DimensionDefinition,{kind:'geometry_circle'}>}[];
}
interface PartFeatures {instance_id:string;features?:Record<string,string[]>}
function inspected(selection:Entity,inspection:Inspection|null){
  return inspection?.revision===selection.revision?inspection.entities?.find(item=>item.reference.revision===selection.revision&&item.reference.instance_id===selection.instance_id&&item.reference.entity_id===selection.entity_id):undefined;
}
function uniqueFeature(selection:Entity,part:PartFeatures){
  const names=Object.entries(part.features||{}).filter(([,entities])=>entities.length===1&&entities[0]===selection.entity_id).map(([name])=>name);
  return names.length===1?names[0]:null;
}
function validReference(entity:GeometryRef|undefined){return entity?.format==='occt-brep-v3-no-display-v1'&&/^[0-9a-f]{64}$/.test(entity.sha256)&&['face','edge'].includes(entity.entity_kind);}
export function selectedAnchor(selection:Entity|null,part:PartFeatures|undefined,inspection:Inspection|null):Anchor|null{
  if(!selection||!part||selection.instance_id!==part.instance_id)return null;
  if(selection.entity_id==='part:0')return {kind:'bounds',instance_id:part.instance_id,fraction:[0.5,0.5,0.5]};
  const feature=uniqueFeature(selection,part);
  if(feature)return {kind:'feature_center',instance_id:part.instance_id,feature};
  const anchor=inspected(selection,inspection)?.anchor;
  return anchor?.kind==='geometry_center'&&anchor.instance_id===selection.instance_id&&validReference(anchor.entity)&&anchor.entity.entity_kind===selection.entity_id.split(':')[0]?anchor:null;
}
export function selectedCircle(selection:Entity|null,part:PartFeatures|undefined,inspection:Inspection|null,measure:'radius'|'diameter'):DimensionDefinition|null{
  if(!selection||!part||selection.instance_id!==part.instance_id)return null;
  const circle=inspected(selection,inspection)?.circular_definition;
  if(circle?.kind!=='geometry_circle'||circle.instance_id!==selection.instance_id||!validReference(circle.entity)||circle.entity.entity_kind!==selection.entity_id.split(':')[0])return null;
  const feature=uniqueFeature(selection,part);
  return feature?{kind:measure,instance_id:part.instance_id,feature}:{...circle,measure};
}
