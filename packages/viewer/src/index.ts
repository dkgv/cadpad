import { Display, Viewer, type Shape, type Shapes } from 'three-cad-viewer';
import 'three-cad-viewer/css';
import * as THREE from 'three';
import {dimensionStroke,moveDimensionStroke,placeDimensionLabels,angleGeometry,dimensionText,placeAnnotation} from './dimension-display';
export {dimensionText} from './dimension-display';
import {setRenderedVisibility} from './visibility';
import {useCadpadHover} from './render-policy';
import {sceneEntity,sameEntity,referenceLetter} from './entities';
export {sceneEntity,sameEntity,referenceLetter} from './entities';
import {revisionBuffers,type BufferDescriptor} from './buffers';
import {shareRenderBuffers,revisionRenderBuffers} from './render-buffers';
import {PickGeometry,scenePicking,pickTopology,pickTopologyInRect,geometryCenter,type SelectionMode,type MeshBuffers} from './picking';
export type {SelectionMode} from './picking';
import { partMotion, manipulationJoint, interpolateSimulation, previewJoint, sliderDelta, hingeDelta, unwrapStep, pathCoordinate, sliderScreenMap, hingeScreenMap, type MotionSnapshot, type JointState, type Ray } from './motion';
export type {MotionSnapshot,JointState} from './motion';
export {recordedFrameAt,partMotion} from './motion';
export {selectedAnchor,selectedCircle} from './references';
export type {Inspection} from './references';

export type Vec3 = [number, number, number];
export interface Entity { revision: string; instance_id: string; entity_id: string }
/** `additive` is a ⌘/Ctrl click that adds or removes a reference; `marquee` is Shift+drag rectangle selection (Bambu-style); `mode` is a pick-mode change. */
export type SelectOrigin='replacement'|'additive'|'mode'|'marquee'|'marquee-add';
export type SelectHandler=(entity:Entity|null,origin?:SelectOrigin,batch?:Entity[])=>void;
type HighlightKind='hover'|'selected'|'reference'|'motion';
export interface Part {
  accuracy?:string;
  id: string; instance_id: string; name: string; position: Vec3;
  bounds: { min: Vec3; max: Vec3; size: Vec3 }; volume: number | null; area: number;
  features?:Record<string,string[]>;
  /** Project-relative component file and line of the show() call that published this part. */
  source?:{file:string;line:number};
  mesh: { buffers: Record<string, BufferDescriptor>; face_ids: (string|null)[]; edge_ids: (string|null)[] };
}
export interface GeometryRef {entity_kind:'face'|'edge';format:'occt-brep-v3-no-display-v1';sha256:string}
export type Anchor = {kind:'local_point';instance_id:string;point:Vec3}|{kind:'bounds';instance_id:string;fraction:Vec3}|{kind:'feature_center';instance_id:string;feature:string}|{kind:'geometry_center';instance_id:string;entity:GeometryRef};
export type DimensionDefinition = {kind:'overall';instance_id:string;axis:'x'|'y'|'z'}|{kind:'published';publication_id:string}|{kind:'distance';start:Anchor;end:Anchor}|{kind:'angle';start:Anchor;vertex:Anchor;end:Anchor}|{kind:'radius'|'diameter';instance_id:string;feature:string}|{kind:'geometry_circle';instance_id:string;entity:GeometryRef;measure:'radius'|'diameter'};
export type DimensionFormat = {unit:'mm'|'cm'|'m'|'in'|'deg'|'rad';precision:number};
export interface SavedAnnotation { id:string;label:string;definition:DimensionDefinition;format?:DimensionFormat;parameter?:string|null;label_offset?:[number,number] }
export interface Annotation { id: string; kind: string; part?: string; end_part?:string;vertex_part?:string;vertex?:Vec3; start?: Vec3; end?: Vec3; value?: number; label: string; parameter?: string | null; saved_id?:string; status?:'resolved'|'broken';error?:{code:string;message:string};definition?:DimensionDefinition;format?:DimensionFormat;label_offset?:[number,number] }
interface ResolvedAnnotation extends Annotation {part:string;start:Vec3;end:Vec3;value:number}
const resolved = (annotation:Annotation): annotation is ResolvedAnnotation => annotation.status!=='broken'&&!!annotation.part&&!!annotation.start&&!!annotation.end&&Number.isFinite(annotation.value);
function segmentPairs(attribute:THREE.BufferAttribute|THREE.InterleavedBufferAttribute){
  const segments:{start:THREE.Vector3;end:THREE.Vector3}[]=[];
  for(let i=0;i+1<attribute.count;i+=2){
    const start=new THREE.Vector3().fromBufferAttribute(attribute,i),end=new THREE.Vector3().fromBufferAttribute(attribute,i+1);
    if(start.distanceToSquared(end)>1e-12)segments.push({start,end});
  }
  return segments;
}
function pairFloats(values:ArrayLike<number>){
  const segments:{start:THREE.Vector3;end:THREE.Vector3}[]=[];
  for(let i=0;i+5<values.length;i+=6){
    const start=new THREE.Vector3(values[i],values[i+1],values[i+2]),end=new THREE.Vector3(values[i+3],values[i+4],values[i+5]);
    if(start.distanceToSquared(end)>1e-12)segments.push({start,end});
  }
  return segments;
}
/** Edges shared by two triangles of a face are internal tessellation. Keep the loop. */
function faceBoundary(geometry:THREE.BufferGeometry){
  const index=geometry.getIndex(),position=geometry.getAttribute('position');
  if(!index||!position)return [];
  const edges=new Map<string,[number,number]>();
  for(let i=0;i+2<index.count;i+=3){
    const tri=[index.getX(i),index.getX(i+1),index.getX(i+2)];
    for(let edge=0;edge<3;edge++){
      const a=tri[edge],b=tri[(edge+1)%3],id=a<b?`${a}:${b}`:`${b}:${a}`;
      if(edges.has(id))edges.delete(id);else edges.set(id,[a,b]);
    }
  }
  return [...edges.values()].flatMap(([a,b])=>{
    const start=new THREE.Vector3().fromBufferAttribute(position,a),end=new THREE.Vector3().fromBufferAttribute(position,b);
    return start.distanceToSquared(end)>1e-12?[{start,end}]:[];
  });
}
export function modelDimensions(model:SceneModel):Annotation[] {
  const saved=(model.annotations||[]).filter(a=>a.saved_id);
  const published=(model.annotations||[]).filter(a=>!a.saved_id&&!saved.some(s=>s.status!=='broken'&&s.definition?.kind==='published'&&s.definition.publication_id===a.id));
  const overall=model.parts.flatMap(part=>(['x','y','z'] as const).map((axis,index):Annotation=>{
    const start=[...part.bounds.min] as Vec3,end=[...start] as Vec3;end[index]=part.bounds.max[index];
    return {id:`overall:${part.instance_id}:${axis}`,kind:'overall',part:part.instance_id,start,end,value:part.bounds.size[index],label:['Width','Depth','Height'][index],definition:{kind:'overall',instance_id:part.instance_id,axis}};
  })).filter(a=>!saved.some(s=>s.status!=='broken'&&s.definition?.kind==='overall'&&s.definition.instance_id===a.part&&s.definition.axis===(a.definition as {axis:string}).axis));
  return [...saved,...published,...overall];
}
export interface SceneModel { parts: Part[]; annotations?: Annotation[] }
export type ArtifactLoader = (handle: string) => Promise<ArrayBuffer>;
export type Scope = 'selected' | 'pinned' | 'all';

/** Only this adapter touches the CAD viewer. Coordinates stay Z-up/mm. */
export class CadViewport {
  private display: Display;
  private viewer: Viewer;
  private host = document.createElement('div');
  private overlay = document.createElement('div');
  private hoverStatusElement:HTMLElement|null=null;
  private motionStatusElement:HTMLElement|null=null;
  private annotationGroup = new THREE.Group();
  private resize: ResizeObserver;
  private parts: Part[] = [];
  private paths = new Map<string, Part>();
  private pickGeometry = new Map<string, PickGeometry>();
  private selectionMode:SelectionMode='face';
  private selectedEntity:Entity|null=null;
  private hovered:Entity|null=null;
  private highlights:{object:THREE.Mesh;kind:HighlightKind}[]=[];
  private references:Entity[]=[];
  private unlettered:Entity[]=[];
  private badgeLayer=document.createElement('div');
  private badges:{element:HTMLElement;anchor:THREE.Vector3;parent:THREE.Object3D;letter:string;instance:string}[]=[];
  private primaryAnchor:{parent:THREE.Object3D;anchor:THREE.Vector3}|null=null;
  private badgeRenderFrame=-1;
  private hoverFrame:number|null=null;
  private hoverEvent:PointerEvent|null=null;
  private revision = '';
  private model: SceneModel = { parts: [] };
  private labels: { element: HTMLElement; anchor: THREE.Vector3; offset:[number,number];leader:ReturnType<typeof dimensionStroke> }[] = [];
  private visuals:{annotation:ResolvedAnnotation;line:ReturnType<typeof dimensionStroke>;leader:ReturnType<typeof dimensionStroke>;element:HTMLElement;anchor:THREE.Vector3;value:number|undefined}[]=[];
  private selected: string | null = null;
  private visible = new Set<string>();
  private dimensions = false;
  private advancedTools=false;
  private labelRenderFrame = -1;
  private localAngleLayouts=new WeakMap<ResolvedAnnotation,ReturnType<typeof angleGeometry>>();
  private scope: Scope = 'selected';
  private loadSequence = 0;
  private revisionBytes=revisionBuffers();
  private pickingPool=scenePicking();
  private pickingWorkspace:string|null=null;
  private disposed = false;
  private suspended = false;
  private cancelBufferPlan:(()=>void)|null=null;
  private presented = false;
  private repairCamera:{position:Vec3;quaternion:[number,number,number,number];target:Vec3;zoom:number;orthographic:boolean}|null=null;
  private renderedBuffers:ReturnType<typeof shareRenderBuffers>|null=null;
  private renderStorage:ReturnType<typeof revisionRenderBuffers>|null=null;
  private preparingRender=false;
  private pointerStart: Vec3 = [0, 0, 0];
  private raycaster = new THREE.Raycaster();
  private onSelect: SelectHandler;
  private motion:MotionSnapshot|null=null;
  private simulationFrame:number|null=null;
  private manipulate=false;
  private onJointTarget:(joint:string,coordinate:number,unit:string,commit:boolean)=>void=()=>{};
  private drag:{pointer:number;joint:JointState;startRay:Ray;startX:number;lastX:number;pathStart:number;mmPerPixel:number;lastRaw:number;angle:number;target:number;screenMap:boolean}|null=null;
  private marquee:{pointer:number;startX:number;startY:number;currentX:number;currentY:number;active:boolean;additive:boolean}|null=null;
  private marqueeElement=document.createElement('div');
  private pressMarker=document.createElement('div');
  private pressedPointer:number|null=null;
  private contextLost=false;

  constructor(private container: HTMLElement, onSelect: SelectHandler, private onAnnotation: (annotation:Annotation)=>void = ()=>{}, private onGraphics:(state:'lost'|'restored')=>void=()=>{}) {
    this.onSelect = onSelect;
    this.host.className='cadpad-viewer-host';
    this.host.style.cssText='position:absolute;inset:0';
    let allocatedDisplay:Display|undefined,allocatedViewer:Viewer|undefined,allocatedResize:ResizeObserver|undefined;
    try{
      container.appendChild(this.host);
      this.display = allocatedDisplay = new Display(this.host, { cadWidth: container.clientWidth, height: container.clientHeight,
        treeWidth: 250, tools: false, measureTools: false, selectTool: false, theme: 'browser',
        externalMeasurementBackend: true, explodeTool: false, zscaleTool: false, zebraTool: false, studioTool: false, glass: true, pinning: false });
      this.viewer = allocatedViewer = new Viewer(this.display, { up: 'Z', ortho: true, axes: false }, changes => {
        // The pinned viewer notifies after building the groups and before its
        // initial paint. Do not change uploaded attributes after render returns.
        if(changes&&Object.prototype.hasOwnProperty.call(changes,'target0')&&this.preparingRender&&!this.renderedBuffers&&this.viewer?.ready&&[...this.paths.keys()].every(path=>this.viewer.nestedGroup.groups[path])){
          this.renderStorage??=revisionRenderBuffers(this.viewer.renderer.domElement);
          this.renderedBuffers=this.renderStorage.attach([...this.paths.keys()].map(path=>this.viewer.nestedGroup.groups[path]));
          this.renderStorage.finishSwap();
        }
      });
      useCadpadHover(this.viewer);
      // Three's listeners are registered by its constructor first. Its restored
      // handler recreates device state before we request a display-only reload.
      this.viewer.renderer.domElement.addEventListener('webglcontextlost',this.graphicsLost);
      this.viewer.renderer.domElement.addEventListener('webglcontextrestored',this.graphicsRestored);
      this.display.cadBody.classList.add('cadpad-viewer-body');
      this.display.cadView.classList.add('cadpad-viewer-canvas');
      this.display.showTools(false);this.display.showToolsPanel(false);this.display.showInfo(false);
      // Appearance sliders are not part of the model. The viewer otherwise keeps
      // the material editor on the canvas and a Material tab in its tool panel.
      this.host.querySelector('.tcv_mat_editor')?.remove();
      this.host.querySelector('.tcv_tab_material')?.parentElement?.remove();
      this.host.querySelector('.tcv_toggles_material')?.remove();
      this.host.querySelector('.tcv_cad_material_container')?.remove();
      // An empty warning chip otherwise sits at the top center of the canvas.
      this.host.querySelector('.tcv_warning_banner')?.remove();
      // Shape filter (All/Vertex/Edge/Face/Solid) duplicates Cadpad's pick modes.
      this.host.querySelector('.tcv_filter_menu')?.remove();
      this.overlay.className = 'dimension-overlay';
      container.appendChild(this.overlay);
      this.badgeLayer.className='reference-badges';
      container.appendChild(this.badgeLayer);
      this.marqueeElement.className='selection-marquee';
      this.marqueeElement.hidden=true;
      container.appendChild(this.marqueeElement);
      this.pressMarker.className='canvas-press-marker';this.pressMarker.hidden=true;this.pressMarker.setAttribute('aria-hidden','true');
      container.appendChild(this.pressMarker);
      container.addEventListener('pointerdown', this.pointerDown,true);
      container.addEventListener('pointerup', this.pointerUp,true);
      container.addEventListener('pointermove',this.pointerMove,true);
      container.addEventListener('pointercancel',this.pointerCancel,true);
      container.addEventListener('pointerleave',this.pointerLeave,true);
      container.addEventListener('wheel',this.pointerLeave,{capture:true,passive:true});
      container.addEventListener('lostpointercapture',this.pointerCancel,true);
      this.resize = allocatedResize = new ResizeObserver(() => this.resizeViewport());
      this.resize.observe(container);
    }catch(error){
      // Keep the first failure while attempting every acquired cleanup. Display
      // exists before Viewer attempts WebGL creation, so its document listeners
      // can be released even when the Viewer constructor never returns.
      this.disposed=true;this.loadSequence++;
      this.detachListeners();
      try{allocatedResize?.disconnect();}catch{}
      try{allocatedViewer?.dispose();}catch{}
      try{allocatedDisplay?.dispose();}catch{}
      this.host.remove();this.overlay.remove();this.badgeLayer.remove();this.marqueeElement.remove();this.pressMarker.remove();
      throw error;
    }
  }

  private resizeViewport(){
    if(this.contextLost||this.suspended)return;
    const width=Math.max(1,this.container.clientWidth),height=Math.max(1,this.container.clientHeight);
    // Viewer chrome floats over the scene. Reserving a band for it, or leaving
    // the viewer toolbar in normal flow, clips the model.
    if(!this.viewer.ready){this.display.setSizes({cadWidth:width,height,glass:true,tools:false});return;}
    this.viewer.resizeCadView(width,250,height,true);
  }

  /** Keep the dependency's full hover readout, but outside the drawing area. */
  setStatusContainer(container:HTMLElement){
    this.hoverStatusElement??=this.host.querySelector<HTMLElement>('.tcv_status_line');
    if(this.hoverStatusElement)container.appendChild(this.hoverStatusElement);
    this.motionStatusElement??=document.createElement('span');
    this.motionStatusElement.className='motion-hover-status';
    container.prepend(this.motionStatusElement);this.updateMotionStatus(null);
  }

  hasScene(workspace:string,revision:string){return this.presented&&workspace===this.pickingWorkspace&&revision===this.revision;}
  /** Logical state can outlive its graphics context. Readiness uses hasScene. */
  sceneState(workspace:string,revision:string){
    return !this.disposed&&this.parts.length>0&&workspace===this.pickingWorkspace&&revision===this.revision?{selection:this.selectedEntity?{...this.selectedEntity}:null,hidden:this.parts.filter(part=>!this.visible.has(part.instance_id)).map(part=>part.instance_id)}:null;
  }
  cancelLoad(){this.loadSequence++;this.cancelBufferPlan?.();this.cancelBufferPlan=null;}

  /** Hidden tabs retain interaction state, not mesh arrays, BVHs or GPU geometry. */
  suspend(){
    if(this.disposed||this.suspended)return;
    if(this.presented)this.rememberCamera();
    this.suspended=true;this.presented=false;this.cancelLoad();
    this.clearPressMarker();this.cancelDrag();this.cancelSimulationFrame();this.cancelHover();
    this.hovered=null;this.clearHighlights();this.clearBadges();this.clearAnnotations();
    this.renderStorage?.dispose();this.renderStorage=null;
    this.renderedBuffers?.dispose();this.renderedBuffers=null;
    for(const geometry of this.pickGeometry.values())geometry.dispose();
    this.pickGeometry.clear();this.pickingPool=scenePicking();this.revisionBytes.clear();
    this.viewer.clear();
    this.viewer.renderer.renderLists.dispose();
    // WebKit also retains the canvas backing buffer after the scene is cleared.
    this.viewer.renderer.setSize(1,1,false);
  }

  private rememberCamera(){
    const camera=this.viewer.getCameraLocationSettings();
    this.repairCamera={position:[...camera.position] as Vec3,quaternion:[...camera.quaternion] as [number,number,number,number],target:[...camera.target] as Vec3,zoom:camera.zoom,orthographic:this.viewer.ortho};
  }
  private graphicsLost=(event:Event)=>{
    event.preventDefault();
    if(this.disposed||this.contextLost)return;
    if(this.presented)this.rememberCamera();
    this.contextLost=true;this.presented=false;this.cancelLoad();
    this.clearPressMarker();this.cancelDrag();this.cancelSimulationFrame();this.cancelHover();this.hovered=null;
    if(this.viewer.hasAnimationLoop)this.viewer.toggleAnimationLoop(false);
    this.clearHighlights();this.clearBadges();this.clearAnnotations();
    this.onGraphics('lost');
  };
  private graphicsRestored=()=>{
    if(this.disposed||!this.contextLost)return;
    this.contextLost=false;
    // A restored context alone does not establish a ready Cadpad scene.
    this.onGraphics('restored');
  };

  async load(model: SceneModel, revision: string, artifact: ArtifactLoader, workspace:string|null=null) {
    if(this.contextLost)throw new Error('The graphics connection is unavailable. Wait for it to return before retrying the view.');
    this.cancelLoad();this.suspended=false;
    const sequence = this.loadSequence;
    const picking = new Map<string, PickGeometry>();
    const preparePicking=workspace&&workspace===this.pickingWorkspace?this.pickingPool:scenePicking();
    const rawBuffers = new Map<string, MeshBuffers>();
    const topology = new Map(model.parts.map(part=>[part.instance_id,part.mesh]));
    const paths = new Map<string, Part>();
    const bufferPlan=this.revisionBytes.begin(workspace,artifact);
    const releasePlan=()=>bufferPlan.discard();this.cancelBufferPlan=releasePlan;
    const shapes = await Promise.all(model.parts.map(async (part, index): Promise<Shapes> => {
      const buffers = Object.fromEntries(await Promise.all(Object.entries(part.mesh.buffers).map(async ([key, info]) => {
        const data = await bufferPlan.load(info);
        if (data.byteLength !== info.bytes || data.byteLength % 4) throw new Error('Invalid mesh artifact');
        return [key, ['triangles', 'triangles_per_face', 'segments_per_edge', 'face_types', 'edge_types'].includes(key) ? new Uint32Array(data) : new Float32Array(data)];
      })));
      const path = `/model/${part.id}`;
      rawBuffers.set(part.instance_id,buffers as MeshBuffers);
      paths.set(path, part);
      return { version: 3, id: path, name: part.name, type: 'shapes', subtype: 'solid',
        shape: { ...buffers, edge_types: Array.from(buffers.edge_types || []) } as Shape,
        state: [1, 1], color: ['#b7c8d2', '#e5ae69', '#94baa6'][index % 3], alpha: 1,
        loc: [part.position, [0, 0, 0, 1]] };
    })).catch(error=>{bufferPlan.discard();throw error;});
    if (sequence !== this.loadSequence || this.disposed){bufferPlan.discard();return;}
    try{for(const [instance,buffers] of rawBuffers)picking.set(instance,preparePicking(buffers,topology.get(instance)!));}
    catch(error){for(const item of picking.values())item.dispose();bufferPlan.discard();throw error;}
    // Read interaction state at the replacement boundary, after asynchronous
    // fetching. Navigation and visibility changes made while loading survive.
    const preserve=!!workspace&&workspace===this.pickingWorkspace&&revision===this.revision&&model.parts.length>0;
    const hidden=preserve?new Set(this.parts.filter(part=>!this.visible.has(part.instance_id)).map(part=>part.instance_id)):new Set<string>();
    if(preserve&&this.presented)this.rememberCamera();
    else if(!preserve)this.repairCamera=null;
    const selected=preserve?this.selectedEntity:null;
    const retainedSelection=selected&&!hidden.has(selected.instance_id)?sceneEntity(model,revision,selected):null;
    const clearSelection=!preserve||!!selected&&!retainedSelection;
    this.presented=false;
    this.clearHighlights();this.clearBadges();this.cancelHover();
    // References are revision scoped; the owner re-sends whatever survives.
    if(revision!==this.revision)this.references=this.references.filter(entity=>entity.revision===revision);
    for(const item of this.pickGeometry.values())item.dispose();
    this.clearAnnotations();
    if(!workspace||workspace!==this.pickingWorkspace){this.renderStorage?.dispose();this.renderStorage=null;}
    this.renderStorage?.beginSwap();
    this.renderedBuffers?.dispose();this.renderedBuffers=null;
    try{if (this.parts.length) this.viewer.clear();}
    catch(error){for(const item of picking.values())item.dispose();this.renderStorage?.finishSwap();bufferPlan.discard();throw error;}
    this.parts = model.parts;
    this.model = model;
    this.paths = paths;
    this.pickGeometry = picking;
    this.pickingPool=preparePicking;this.pickingWorkspace=workspace;
    this.revision = revision;
    this.clearPressMarker();this.cancelDrag();this.cancelSimulationFrame();if(!preserve)this.motion=null;
    this.selectedEntity=retainedSelection;this.selected=retainedSelection?.instance_id??null;this.hovered=null;
    if(clearSelection)this.onSelect(null,'replacement');
    this.visible = new Set(model.parts.filter(part=>!hidden.has(part.instance_id)).map(part=>part.instance_id));
    if (!shapes.length){this.renderStorage?.finishSwap();this.setAdvancedTools(this.advancedTools);bufferPlan.commit();bufferPlan.discard();if(this.cancelBufferPlan===releasePlan)this.cancelBufferPlan=null;return;}
    this.preparingRender=true;
    try{this.viewer.render({ version: 3, id: '/model', name: 'Model', parts: shapes,
      loc: [[0, 0, 0], [0, 0, 0, 1]] }, { metalness: 0.15, roughness: 0.65 },
      { up: 'Z', ortho: true, control: 'orbit', axes: false, grid: [false, false, false] });
    }catch(error){bufferPlan.discard();throw error;}finally{this.preparingRender=false;this.renderStorage?.finishSwap();}
    if(this.contextLost){bufferPlan.discard();throw new Error('The graphics connection was interrupted while preparing the view.');}
    this.presented=true;
    this.viewer.scene.add(this.annotationGroup);
    // A repair resumes at the current canonical pose; it must not replay a
    // paused simulation's previous interpolation interval.
    if(this.motion)this.applyPoses(this.motion.poses);
    if(this.repairCamera){const camera=this.repairCamera;this.viewer.switchCamera(camera.orthographic);this.viewer.setCameraLocationSettings(camera.position,camera.quaternion,camera.target,camera.zoom);}
    else this.viewer.presetCamera('iso');
    this.viewer.glassMode(true);
    this.setAdvancedTools(this.advancedTools);
    for(const [path,part] of this.paths)if(!this.visible.has(part.instance_id))setRenderedVisibility(this.viewer,path,false);
    this.highlight(this.selectedEntity,false);
    this.drawMotionHints();
    this.drawReferences();
    this.refreshAnnotations();
    this.viewer.update(true);
    bufferPlan.commit();bufferPlan.discard();if(this.cancelBufferPlan===releasePlan)this.cancelBufferPlan=null;
    this.repairCamera=null;
  }

  private ray(event:PointerEvent):Ray{
    const rect = this.viewer.renderer.domElement.getBoundingClientRect();
    this.raycaster.setFromCamera(new THREE.Vector2((event.clientX-rect.left)/rect.width*2-1, -(event.clientY-rect.top)/rect.height*2+1), this.viewer.camera.getCamera());
    return {origin:this.raycaster.ray.origin.toArray() as Vec3,direction:this.raycaster.ray.direction.toArray() as Vec3};
  }
  private pick(event:PointerEvent,mode:SelectionMode=this.selectionMode):Entity|null {
    if(!this.presented)return null;
    const rect=this.viewer.renderer.domElement.getBoundingClientRect();
    const hit=pickTopology(this.viewer.camera.getCamera(),new THREE.Vector2((event.clientX-rect.left)/rect.width*2-1,-(event.clientY-rect.top)/rect.height*2+1),rect,mode,this.pickInstances());
    return hit?{revision:this.revision,...hit}:null;
  }
  setSelectionMode(mode:SelectionMode){
    if(this.selectionMode===mode)return;
    this.selectionMode=mode;this.cancelHover();this.setHover(null);
    if(this.selectedEntity&&!this.selectedEntity.entity_id.startsWith(`${mode}:`)){this.selectEntity(null);this.onSelect(null,'mode');}
  }
  private clearHighlights(kind?:HighlightKind){
    this.highlights=this.highlights.filter(item=>{
      if(kind!==undefined&&item.kind!==kind)return true;
      item.object.removeFromParent();item.object.geometry.dispose();
      for(const material of Array.isArray(item.object.material)?item.object.material:[item.object.material])material.dispose();
      return false;
    });
  }
  private selectionStroke(segments:{start:THREE.Vector3;end:THREE.Vector3}[],color:string,width:number){
    if(!segments.length)return null;
    const mesh=dimensionStroke(segments);
    mesh.material.uniforms.color.value.set(color);mesh.material.uniforms.width.value=width;mesh.renderOrder=20;
    return mesh;
  }
  private addHighlight(parent:THREE.Object3D,object:THREE.Mesh|null,kind:HighlightKind){
    if(!object)return;
    object.raycast=()=>{};parent.add(object);this.highlights.push({object,kind});
    if(kind!=='hover'){
      // Badges track camera and pose changes from whichever selected highlight
      // draws first in a frame; never culled so offscreen badges still hide.
      object.frustumCulled=false;
      const before=object.onBeforeRender;
      object.onBeforeRender=(...args)=>{const frame=args[0].info.render.frame;if(this.badgeRenderFrame!==frame){this.badgeRenderFrame=frame;this.projectBadges();}before.apply(object,args);};
    }
  }
  private highlightTarget(entity:Entity|null){
    if(!this.presented||!entity||entity.revision!==this.revision||!this.visible.has(entity.instance_id))return null;
    const entry=[...this.paths].find(([,part])=>part.instance_id===entity.instance_id);
    const geometry=this.pickGeometry.get(entity.instance_id);if(!entry||!geometry)return null;
    const parent=this.viewer.nestedGroup.groups[entry[0]];if(!parent)return null;
    const [kind,indexText]=entity.entity_id.split(':');
    return {parent,geometry,kind:kind as SelectionMode,index:Number(indexText)};
  }
  /** Draws one entity and returns its part-local center for badge placement. */
  private drawEntity(entity:Entity|null,kindOf:HighlightKind):{parent:THREE.Object3D;anchor:THREE.Vector3}|null{
    const target=this.highlightTarget(entity);if(!target)return null;
    const {parent,geometry,kind,index}=target,hover=kindOf==='hover',motionHint=kindOf==='motion';
    const capability=entity&&this.motion?partMotion(this.motion,entity.instance_id):null;
    const color=motionHint?'#0b9b83':hover&&this.manipulate?(capability?.kind==='draggable'?'#0b9b83':capability?.kind==='linked'?'#b88830':'#8591a0'):hover?'#eab85d':'#2484c5';
    const mesh=geometry.highlight(kind,index);if(!mesh)return null;
    const anchor=geometryCenter(mesh);
    if(kind==='edge'||kind==='part'&&!geometry.buffers.triangles?.length){
      this.addHighlight(parent,this.selectionStroke(segmentPairs(mesh.getAttribute('position') as THREE.BufferAttribute),color,kind==='edge'?3.2:2.2),kindOf);
      mesh.dispose();
      return {parent,anchor};
    }
    const fill=new THREE.Mesh(mesh,new THREE.MeshBasicMaterial({color,transparent:true,opacity:motionHint?0.10:kind==='face'?(hover?0.38:0.5):(hover?0.22:0.32),depthWrite:false,side:THREE.DoubleSide,polygonOffset:true,polygonOffsetFactor:-2,polygonOffsetUnits:-2}));
    fill.renderOrder=motionHint?8:10;this.addHighlight(parent,fill,kindOf);
    this.addHighlight(parent,this.selectionStroke(kind==='face'?faceBoundary(mesh):pairFloats(geometry.buffers.edges??[]),color,kind==='face'?2.2:1.7),kindOf);
    return {parent,anchor};
  }
  private drawMotionHints(){
    this.clearHighlights('motion');
    if(!this.presented||!this.manipulate||!this.motion||this.motion.mode!=='manipulate'||this.motion.playback)return;
    for(const part of this.parts)if(manipulationJoint(this.motion,part.instance_id))this.drawEntity({revision:this.revision,instance_id:part.instance_id,entity_id:'part:0'},'motion');
  }
  private updateMotionStatus(entity:Entity|null){
    if(this.hoverStatusElement)this.hoverStatusElement.hidden=this.manipulate;
    const element=this.motionStatusElement;if(!element)return;
    element.hidden=!this.manipulate;
    if(!this.manipulate)return;
    if(!entity||!this.motion){element.textContent='Teal parts can be dragged · fixed parts stay in place';return;}
    const name=this.parts.find(part=>part.instance_id===entity.instance_id)?.name||entity.instance_id;
    const capability=partMotion(this.motion,entity.instance_id),joint=capability.joint;
    const action=joint?.kind==='revolute'?'rotate':joint?.kind==='path'?'move along its track':'slide';
    element.textContent=capability.kind==='draggable'?`${name} · Drag to ${action}`:capability.kind==='linked'?`${name} · Follows the mechanism · drag a teal part to move it`:`${name} · Fixed · cannot be dragged`;
  }
  private highlight(entity:Entity|null,hover:boolean){
    this.clearHighlights(hover?'hover':'selected');
    const drawn=this.drawEntity(entity,hover?'hover':'selected');
    if(!hover){this.primaryAnchor=drawn;this.drawReferences();}
  }
  /**
   * References are everything the user has selected for the assistant. The
   * primary selection is drawn by highlight(); the rest share its color, and
   * letter badges appear once more than one entity is referenced.
   */
  setReferences(entities:Entity[],unlettered:Entity[]=[]){
    this.references=entities.map(entity=>({...entity}));
    this.unlettered=unlettered.map(entity=>({...entity}));
    this.drawReferences();
    if(this.presented)this.viewer.update(true);
  }
  private clearBadges(){this.badgeLayer.replaceChildren();this.badges=[];this.primaryAnchor=null;}
  private drawReferences(){
    this.clearHighlights('reference');
    const primary=this.primaryAnchor;this.clearBadges();this.primaryAnchor=primary;
    // Letters count only references the assistant will receive, so badges
    // always match the letters in the sent message.
    const isLettered=(entity:Entity)=>!this.unlettered.some(item=>sameEntity(item,entity));
    const lettered=this.references.filter(isLettered).length>1;let next=0;
    this.references.forEach(entity=>{
      const primary=sameEntity(entity,this.selectedEntity);
      const drawn=primary?this.primaryAnchor:this.drawEntity(entity,'reference');
      if(!isLettered(entity))return;
      const index=next++;
      if(!drawn||!lettered)return;
      const letter=referenceLetter(index),element=document.createElement('span');
      element.className='reference-badge';element.textContent=letter;element.style.visibility='hidden';
      this.badgeLayer.appendChild(element);this.badges.push({element,anchor:drawn.anchor,parent:drawn.parent,letter,instance:entity.instance_id});
    });
    this.badgeRenderFrame=-1;this.projectBadges();
  }
  private badgePoint(anchor:THREE.Vector3,parent:THREE.Object3D,width:number,height:number){
    const camera=this.viewer.camera.getCamera(),world=anchor.clone().applyMatrix4(parent.matrixWorld);
    if(world.clone().applyMatrix4(camera.matrixWorldInverse).z>=0)return null;
    const p=world.project(camera);
    if(Math.abs(p.x)>1.05||Math.abs(p.y)>1.05)return null;
    return {x:(p.x+1)*width/2,y:(1-p.y)*height/2};
  }
  private projectBadges(){
    if(!this.presented||!this.badges.length)return;
    const canvas=this.viewer.renderer.domElement.getBoundingClientRect(),host=this.container.getBoundingClientRect();
    for(const badge of this.badges){
      const point=this.badgePoint(badge.anchor,badge.parent,canvas.width,canvas.height);
      badge.element.style.visibility=point&&this.visible.has(badge.instance)?'visible':'hidden';
      if(point)badge.element.style.transform=`translate(${point.x+canvas.left-host.left}px, ${point.y+canvas.top-host.top}px) translate(-50%, -50%)`;
    }
  }
  private setHover(entity:Entity|null){
    if(entity?.instance_id===this.hovered?.instance_id&&entity?.entity_id===this.hovered?.entity_id)return;
    this.hovered=entity;this.updateMotionStatus(entity);this.highlight(sameEntity(entity,this.selectedEntity)||this.references.some(item=>sameEntity(item,entity))?null:entity,true);if(this.presented)this.viewer.update(true);
  }
  private cancelHover(){if(this.hoverFrame!==null)cancelAnimationFrame(this.hoverFrame);this.hoverFrame=null;this.hoverEvent=null;}
  private pointerLeave=()=>{this.cancelHover();this.setHover(null);this.updateMotionStatus(null);if(!this.drag){this.manipulationCursor('');if(!this.marquee)this.clearPressMarker();}};
  private manipulationCursor(cursor:string){this.host.style.cursor=cursor;this.viewer.renderer.domElement.style.cursor=cursor;}

  private pickInstances(){
    return [...this.paths].filter(([,part])=>this.visible.has(part.instance_id)).flatMap(([path,part])=>{
      const group=this.viewer.nestedGroup.groups[path],geometry=this.pickGeometry.get(part.instance_id);
      return group&&geometry?[{instance_id:part.instance_id,matrix:group.matrixWorld,geometry}]:[];
    });
  }
  private showPressMarker(event:PointerEvent){
    const bounds=this.container.getBoundingClientRect();
    this.pressedPointer=event.pointerId;this.pressMarker.hidden=false;this.pressMarker.dataset.state='pressed';
    this.pressMarker.style.left=`${event.clientX-bounds.left}px`;this.pressMarker.style.top=`${event.clientY-bounds.top}px`;
  }
  private clearPressMarker(){this.pressedPointer=null;this.pressMarker.hidden=true;}
  private pointerDown = (event: PointerEvent) => {
    if(!this.presented)return;
    if((event.target as Element).closest?.('.dimension-inspect'))return;
    if(this.drag||this.marquee){event.preventDefault();event.stopPropagation();return;}
    this.cancelHover();this.setHover(null);
    this.pointerStart = [event.clientX,event.clientY,performance.now()];
    if(!(event.target as Element).closest?.('button,input,select,.tcv_cad_toolbar,.tcv_cad_navigation'))this.showPressMarker(event);
    // Bambu-style: Shift + left drag draws a selection rectangle. Capture early so
    // the orbit controller does not rotate the view for the same gesture.
    if(event.button===0&&event.shiftKey&&this.parts.length){
      event.preventDefault();event.stopPropagation();
      this.container.setPointerCapture(event.pointerId);
      this.marquee={pointer:event.pointerId,startX:event.clientX,startY:event.clientY,currentX:event.clientX,currentY:event.clientY,active:false,additive:event.metaKey||event.ctrlKey};
      return;
    }
    if(!this.manipulate||event.button!==0||!this.motion||this.motion.mode!=='manipulate'||this.motion.playback)return;
    // Move mode owns primary dragging, including misses. Never silently hand a
    // failed part pick to the camera controller. Topology filters are for inspect.
    event.preventDefault();event.stopPropagation();
    const entity=this.pick(event,'part'),joint=entity?manipulationJoint(this.motion,entity.instance_id):null;
    if(!entity||!joint){this.pressMarker.dataset.state='blocked';this.updateMotionStatus(entity);this.manipulationCursor(entity?'not-allowed':'default');return;}
    this.updateMotionStatus(entity);
    this.pressMarker.dataset.state='moving';
    this.manipulationCursor('grabbing');this.container.setPointerCapture(event.pointerId);
    const camera=this.viewer.camera.getCamera(),startRay=this.ray(event);
    const origin=new THREE.Vector3(...joint.origin_mm).project(camera),pixel=origin.clone();
    pixel.y+=2/Math.max(1,this.viewer.renderer.domElement.clientHeight);
    const scale=pixel.unproject(camera).distanceTo(origin.unproject(camera));const mmPerPixel=Number.isFinite(scale)&&scale>0?scale:1;
    const screenMap=joint.kind==='prismatic'?sliderScreenMap(startRay,joint.origin_mm,joint.axis_world):joint.kind==='path'?false:hingeScreenMap(startRay,joint.origin_mm,joint.axis_world);
    this.drag={pointer:event.pointerId,joint,startRay,startX:event.clientX,lastX:event.clientX,pathStart:joint.path?pathCoordinate(startRay,joint.path.samples,joint.coordinate,0,mmPerPixel):joint.coordinate,mmPerPixel,lastRaw:0,angle:0,target:joint.coordinate,screenMap};
    this.selectEntity(entity);this.onSelect(entity);
  };
  private pointerMove=(event:PointerEvent)=>{
    if(!this.presented)return;
    if(this.marquee&&event.pointerId===this.marquee.pointer){
      event.preventDefault();event.stopPropagation();
      this.marquee.currentX=event.clientX;this.marquee.currentY=event.clientY;
      if(!this.marquee.active&&Math.hypot(event.clientX-this.marquee.startX,event.clientY-this.marquee.startY)>4){
        this.marquee.active=true;this.marqueeElement.hidden=false;
      }
      if(this.marquee.active)this.updateMarqueeElement();
      return;
    }
    const drag=this.drag;if(!drag){
      if(event.buttons||(event.target as Element).closest?.('.dimension-inspect')){this.cancelHover();this.setHover(null);return;}
      this.hoverEvent=event;
      if(this.hoverFrame===null)this.hoverFrame=requestAnimationFrame(()=>{this.hoverFrame=null;const pointer=this.hoverEvent;this.hoverEvent=null;if(pointer&&!this.disposed&&this.parts.length){
        const entity=this.pick(pointer,this.manipulate?'part':this.selectionMode);
        this.setHover(entity);
        if(this.manipulate)this.manipulationCursor(entity&&this.motion&&manipulationJoint(this.motion,entity.instance_id)?'grab':entity?'not-allowed':'default');
      }});
      return;
    }
    if(event.pointerId!==drag.pointer)return;
    event.preventDefault();event.stopPropagation();const ray=this.ray(event),dx=event.clientX-drag.startX;
    let target=drag.target;
    if(drag.joint.kind==='path'&&drag.joint.path)target=drag.joint.coordinate+pathCoordinate(ray,drag.joint.path.samples,drag.target-drag.joint.coordinate+drag.pathStart,event.clientX-drag.lastX,drag.mmPerPixel)-drag.pathStart;
    else if(drag.joint.kind==='prismatic'){
      const delta=sliderDelta(drag.startRay,ray,drag.joint.origin_mm,drag.joint.axis_world,dx,drag.mmPerPixel,drag.screenMap?'screen':'ray');
      if(delta!==null)target=drag.joint.coordinate+delta;
    }else{
      const delta=hingeDelta(drag.startRay,ray,drag.joint.origin_mm,drag.joint.axis_world,dx,drag.mmPerPixel,drag.screenMap?'screen':'ray');
      if(delta.angle!==null){
        if(delta.fallback){drag.angle=delta.angle;drag.lastRaw=0;}
        else{drag.angle+=unwrapStep(drag.lastRaw,delta.angle);drag.lastRaw=delta.angle;}
      }
      target=drag.joint.coordinate+drag.angle;
    }
    drag.lastX=event.clientX;
    if(!this.motion)return;
    const preview=previewJoint(this.motion,drag.joint.id,target);
    if(preview.coordinate===drag.target)return;
    drag.target=preview.coordinate;
    // Open-tree previews stay local even when collision is configured; constrained
    // assemblies wait for the interactive server pose (check_collision=false).
    if(!this.motion.constraints&&this.motion.mode!=='simulate'&&!this.motion.playback)this.applyPoses(preview.poses);
    this.onJointTarget(drag.joint.id,preview.coordinate,drag.joint.unit,false);
  };
  private pointerUp = (event: PointerEvent) => {
    if(this.pressedPointer===event.pointerId)this.clearPressMarker();
    if(!this.presented)return;
    if((event.target as Element).closest?.('.dimension-inspect'))return;
    if(this.marquee?.pointer===event.pointerId){
      event.preventDefault();event.stopPropagation();
      const marquee=this.marquee;this.cancelMarquee();
      if(marquee.active){this.commitMarquee(marquee);return;}
      // Shift+click without a drag keeps additive multi-select.
      if(!this.parts.length)return;
      const entity=this.pick(event);
      if(entity)this.onSelect(entity,'additive');
      return;
    }
    if(this.drag?.pointer===event.pointerId){event.preventDefault();event.stopPropagation();const drag=this.drag;this.cancelDrag();this.onJointTarget(drag.joint.id,drag.target,drag.joint.unit,true);return;}
    if(Math.hypot(event.clientX-this.pointerStart[0],event.clientY-this.pointerStart[1])>4||event.button!==0||!this.parts.length)return;
    const entity=this.pick(event),additive=event.metaKey||event.ctrlKey;
    // Additive clicks only report; the owner decides the new primary and references.
    if(additive){this.onSelect(entity,'additive');return;}
    this.selectEntity(entity);this.onSelect(entity);
  };
  private pointerCancel=(event:PointerEvent)=>{
    if(this.pressedPointer===event.pointerId)this.clearPressMarker();
    if(this.marquee?.pointer===event.pointerId){this.cancelMarquee();return;}
    if(this.drag?.pointer===event.pointerId){const drag=this.drag;this.cancelDrag();this.onJointTarget(drag.joint.id,drag.target,drag.joint.unit,true);if(this.motion)this.applyPoses(this.motion.poses);}
  };
  private cancelDrag(){
    const wasDragging=!!this.drag;
    if(wasDragging)this.clearPressMarker();
    if(this.drag&&this.container.hasPointerCapture(this.drag.pointer))this.container.releasePointerCapture(this.drag.pointer);
    this.drag=null;
    this.manipulationCursor(this.manipulate?'grab':'');
    if(wasDragging&&this.presented)this.updateAnnotationPoses();
  }
  private cancelMarquee(){
    if(this.marquee)this.clearPressMarker();
    if(this.marquee&&this.container.hasPointerCapture(this.marquee.pointer))this.container.releasePointerCapture(this.marquee.pointer);
    this.marquee=null;this.marqueeElement.hidden=true;
  }
  private updateMarqueeElement(){
    if(!this.marquee)return;
    const host=this.container.getBoundingClientRect();
    const left=Math.min(this.marquee.startX,this.marquee.currentX)-host.left;
    const top=Math.min(this.marquee.startY,this.marquee.currentY)-host.top;
    const width=Math.abs(this.marquee.currentX-this.marquee.startX);
    const height=Math.abs(this.marquee.currentY-this.marquee.startY);
    Object.assign(this.marqueeElement.style,{left:`${left}px`,top:`${top}px`,width:`${width}px`,height:`${height}px`});
  }
  private commitMarquee(marquee:{startX:number;startY:number;currentX:number;currentY:number;additive:boolean}){
    if(!this.presented||!this.parts.length)return;
    const canvas=this.viewer.renderer.domElement.getBoundingClientRect();
    const left=Math.min(marquee.startX,marquee.currentX)-canvas.left;
    const right=Math.max(marquee.startX,marquee.currentX)-canvas.left;
    const top=Math.min(marquee.startY,marquee.currentY)-canvas.top;
    const bottom=Math.max(marquee.startY,marquee.currentY)-canvas.top;
    const hits=pickTopologyInRect(this.viewer.camera.getCamera(),{left,top,right,bottom},canvas,this.selectionMode,this.pickInstances())
      .map(hit=>({revision:this.revision,...hit}));
    const primary=hits[hits.length-1]??null;
    this.selectEntity(primary);
    this.onSelect(primary,marquee.additive?'marquee-add':'marquee',hits);
  }
  setManipulation(enabled:boolean,onTarget:(joint:string,coordinate:number,unit:string,commit:boolean)=>void){this.manipulate=enabled;this.onJointTarget=onTarget;this.cancelHover();this.setHover(null);this.host.dataset.manipulating=String(enabled);this.drawMotionHints();this.updateMotionStatus(null);if(this.presented)this.viewer.update(true);this.manipulationCursor(enabled?'default':'');if(!enabled){this.cancelDrag();if(this.motion)this.applyPoses(this.motion.poses);}}
  private cancelSimulationFrame(){if(this.simulationFrame!==null)cancelAnimationFrame(this.simulationFrame);this.simulationFrame=null;}
  setMotion(snapshot:MotionSnapshot){
    if(snapshot.revision!==this.revision||this.pickingWorkspace&&snapshot.workspace_id!==this.pickingWorkspace||this.motion&&snapshot.pose_version<this.motion.pose_version)return;
    if(!this.presented){this.motion=snapshot;this.cancelSimulationFrame();return;}
    if(this.drag&&(snapshot.playback||snapshot.actor!=='local-user'||this.motion?.joints.some(joint=>joint.id!==this.drag!.joint.id&&joint.drivable!==false&&snapshot.joints.find(next=>next.id===joint.id)?.coordinate!==joint.coordinate)))this.cancelDrag();
    if(this.motion?.pose_version===snapshot.pose_version&&this.motion.revision===snapshot.revision)return;
    const advancing=this.motion?.simulation?.current.tick!==snapshot.simulation?.current.tick;
    const definitionsChanged=!this.motion||this.motion.mode!==snapshot.mode||!!this.motion.playback!==!!snapshot.playback||this.motion.joints.length!==snapshot.joints.length||snapshot.joints.some((joint,index)=>{const old=this.motion?.joints[index];return !old||old.id!==joint.id||old.parent!==joint.parent||old.child!==joint.child||old.kind!==joint.kind||old.drivable!==joint.drivable;});
    this.motion=snapshot;
    if(this.manipulate&&definitionsChanged)this.drawMotionHints();
    this.cancelSimulationFrame();
    if(snapshot.mode==='simulate'&&snapshot.simulation&&advancing){
      const start=performance.now(),duration=snapshot.simulation.timestep_s*1000;
      const render=(time:number)=>{this.simulationFrame=null;if(this.disposed||this.motion!==snapshot)return;const alpha=Math.min(1,(time-start)/duration);this.applyPoses(interpolateSimulation(snapshot,alpha));if(alpha<1)this.simulationFrame=requestAnimationFrame(render);};
      render(start);return;
    }
    if(this.drag&&!snapshot.constraints&&snapshot.mode!=='simulate'&&!snapshot.playback)this.applyPoses(previewJoint(snapshot,this.drag.joint.id,this.drag.target).poses);else this.applyPoses(snapshot.poses);
  }
  previewCoordinate(joint:string,coordinate:number){if(this.motion&&!this.motion.constraints&&this.motion.mode!=='simulate'&&!this.motion.playback)this.applyPoses(previewJoint(this.motion,joint,coordinate).poses);}
  private applyPoses(poses:MotionSnapshot['poses']){
    if(!this.presented)return;
    this.cancelHover();this.setHover(null);
    for(const [path,part] of this.paths){const pose=poses[part.instance_id],group=this.viewer.nestedGroup.groups[path];if(!pose||!group)continue;group.position.fromArray(pose.position);group.quaternion.fromArray(pose.quaternion);group.updateMatrixWorld(true);}
    // Annotation projection is deferred while dragging so joint motion stays on the pointer.
    if(!this.drag)this.updateAnnotationPoses();
    if(this.parts.length)this.viewer.update(true);
  }

  select(instance: string | null) {this.selectEntity(instance?{revision:this.revision,instance_id:instance,entity_id:'part:0'}:null);}
  selectEntity(entity:Entity|null){
    this.cancelHover();this.setHover(null);this.selectedEntity=entity;
    this.selected=entity?.instance_id??null;
    this.highlight(entity,false);
    this.refreshAnnotations();
    if (this.presented) this.viewer.update(true);
  }
  setDimensions(visible: boolean, scope: Scope) { this.dimensions = visible; this.scope = scope; this.refreshAnnotations(); if (this.presented) this.viewer.update(true); }
  setVisible(instance: string, visible: boolean) {
    // Opening an assembly replays visibility for every object. Most are already
    // visible: avoid N full scene redraws and N scans of the assembly on load.
    if(this.visible.has(instance)===visible)return;
    visible ? this.visible.add(instance) : this.visible.delete(instance);
    if(this.presented)for (const [path, part] of this.paths) if (part.instance_id === instance) setRenderedVisibility(this.viewer,path,visible);
    if(!visible&&this.selected===instance){this.selectEntity(null);this.onSelect(null);}
    else if(visible&&this.selected===instance)this.highlight(this.selectedEntity,false);
    else this.drawReferences();
    this.setHover(null);this.drawMotionHints();this.refreshAnnotations(); if(this.presented)this.viewer.update(true);
  }
  setAdvancedTools(visible:boolean){
    this.advancedTools=visible;
    const shown=visible&&this.presented;
    this.container.classList.toggle('advanced-view',shown);
    if(this.presented)this.viewer.showTools(shown);
    // Status/version/axes text in the info column duplicates the canvas chrome.
    this.display.showTools(shown);this.display.showToolsPanel(shown);this.display.showInfo(false);
    this.resizeViewport();
  }
  resetView(){this.pointerLeave();if(this.presented)this.viewer.reset();}
  view(direction: 'iso' | 'front' | 'rear' | 'top' | 'bottom' | 'left' | 'right' | Vec3) {
    this.pointerLeave();if(!this.presented)return;
    if(typeof direction==='string'){
      const directions:Record<string,Vec3>={front:[0,-1,0],rear:[0,1,0],left:[-1,0,0],right:[1,0,0],top:[0,0,1],bottom:[0,0,-1],iso:[1,-1,1]};
      direction=directions[direction];
    }
    const current=this.viewer.getCameraLocationSettings();
    const target=new THREE.Vector3(...current.target);
    const distance=new THREE.Vector3(...current.position).distanceTo(target);
    const position=new THREE.Vector3(...direction).normalize().multiplyScalar(distance).add(target);
    const rotation=new THREE.Matrix4().lookAt(position,target,new THREE.Vector3(0,0,1));
    const quaternion=new THREE.Quaternion().setFromRotationMatrix(rotation);
    this.viewer.setCameraLocationSettings(position.toArray(),quaternion.toArray(),current.target as Vec3,null);
  }
  /** Live camera rotation for chrome that mirrors the view (ViewCube). */
  cameraQuaternion():[number,number,number,number]|null{
    if(!this.presented)return null;
    const quaternion=this.viewer.getCameraLocationSettings().quaternion;
    return [quaternion[0],quaternion[1],quaternion[2],quaternion[3]];
  }
  setPose(instance: string, position: Vec3, quaternion: [number, number, number, number]) {
    for (const [path, part] of this.paths) if (part.instance_id === instance) {
      const group = this.viewer.nestedGroup.groups[path];
      group.position.fromArray(position); group.quaternion.fromArray(quaternion); group.updateMatrixWorld(true);
    }
    this.updateAnnotationPoses(); this.viewer.update(true);
  }
  capture(options: {width?:number;height?:number;dimensions?:boolean;camera_preset?:'iso'|'front'|'rear'|'top'|'bottom'|'left'|'right';camera?:{position:Vec3;quaternion:[number,number,number,number];target:Vec3;zoom:number;orthographic:boolean}} = {}) {
    if (!this.presented) throw Object.assign(new Error('No committed geometry is displayed'),{code:'VIEW_NOT_READY'});
    const savedCamera=this.viewer.getCameraLocationSettings(), savedOrtho=this.viewer.ortho;
    const savedDimensions=this.dimensions, savedScope=this.scope;
    try {
      // Captures are tagged with the canonical pose version, never an
      // intermediate display interpolation or an unvalidated drag preview.
      this.cancelSimulationFrame();if(this.motion)this.applyPoses(this.motion.poses);
      if(options.camera){const camera=options.camera;this.viewer.switchCamera(camera.orthographic);this.viewer.setCameraLocationSettings(camera.position,camera.quaternion,camera.target,camera.zoom);}
      else if(options.camera_preset)this.viewer.presetCamera(options.camera_preset);
      this.setDimensions(options.dimensions??false,'all');this.viewer.update(true);
      if(this.contextLost)throw Object.assign(new Error('The graphics connection was interrupted during capture. Retry when the view returns.'),{code:'VIEW_NOT_READY'});
      const source=this.viewer.renderer.domElement;
      const scale=Math.min((options.width??1024)/source.width,(options.height??768)/source.height,1);
      const output=document.createElement('canvas');output.width=Math.max(1,Math.floor(source.width*scale));output.height=Math.max(1,Math.floor(source.height*scale));
      const context=output.getContext('2d');if(!context)throw new Error('PNG capture is unavailable');
      context.drawImage(source,0,0,output.width,output.height);
      // HTML labels are projected in CSS pixels. Composite them explicitly so
      // dimension screenshots include the same labels as the live viewport.
      const rect=source.getBoundingClientRect();
      for(const {element} of this.labels){
        if(element.style.visibility==='hidden'||element.style.display==='none')continue;
        const label=element.getBoundingClientRect();
        const x=(label.left-rect.left)*output.width/rect.width, y=(label.top-rect.top)*output.height/rect.height;
        const w=label.width*output.width/rect.width,h=label.height*output.height/rect.height;
        const style=getComputedStyle(element),pixelScale=output.width/rect.width;
        context.fillStyle=style.backgroundColor;context.strokeStyle=style.borderColor;context.lineWidth=Math.max(.5,parseFloat(style.borderWidth)*pixelScale);
        context.beginPath();context.roundRect(x,y,w,h,parseFloat(style.borderRadius)*pixelScale);context.fill();context.stroke();
        context.font=`${style.fontWeight} ${parseFloat(style.fontSize)*pixelScale}px ${style.fontFamily}`;context.fillStyle=style.color;context.textBaseline='middle';context.fillText(element.textContent||'',x+(parseFloat(style.paddingLeft)+1)*pixelScale,y+h/2);
      }
      // Reference badges use the capture camera, not the live DOM positions.
      const pixelScale=output.width/Math.max(1,rect.width);
      for(const badge of this.badges){
        const point=this.badgePoint(badge.anchor,badge.parent,output.width,output.height);if(!point)continue;
        const radius=10*pixelScale;
        context.beginPath();context.arc(point.x,point.y,radius,0,Math.PI*2);context.fillStyle='#1f6fb5';context.fill();
        context.lineWidth=2*pixelScale;context.strokeStyle='#ffffff';context.stroke();
        context.font=`700 ${12*pixelScale}px -apple-system,BlinkMacSystemFont,sans-serif`;context.fillStyle='#ffffff';context.textAlign='center';context.textBaseline='middle';
        context.fillText(badge.letter,point.x,point.y+.5*pixelScale);context.textAlign='start';
      }
      return {image:output.toDataURL('image/png').split(',')[1],camera:{...this.viewer.getCameraLocationSettings(),orthographic:this.viewer.ortho},revision:this.revision,pose_version:this.motion?.pose_version||0,visible_part_ids:[...this.visible]};
    }finally{
      if(this.contextLost){
        // A capture's temporary camera must not become the user's recovery view.
        this.repairCamera={position:[...savedCamera.position] as Vec3,quaternion:[...savedCamera.quaternion] as [number,number,number,number],target:[...savedCamera.target] as Vec3,zoom:savedCamera.zoom,orthographic:savedOrtho};
        this.dimensions=savedDimensions;this.scope=savedScope;
      }else{
        this.viewer.switchCamera(savedOrtho);this.viewer.setCameraLocationSettings(savedCamera.position as Vec3,savedCamera.quaternion as [number,number,number,number],savedCamera.target as Vec3,savedCamera.zoom);
        this.setDimensions(savedDimensions,savedScope);this.viewer.update(true);
      }
    }
  }

  private clearAnnotations() {
    this.annotationGroup.traverse(object => {
      if (object instanceof THREE.Line || object instanceof THREE.Mesh) {
        object.geometry.dispose();
        const materials = Array.isArray(object.material) ? object.material : [object.material];
        materials.forEach(material => material.dispose());
      }
    });
    this.labelRenderFrame=-1;this.localAngleLayouts=new WeakMap();
    this.annotationGroup.clear();
    this.overlay.replaceChildren(); this.labels = [];this.visuals=[];
  }
  private refreshAnnotations() {
    this.clearAnnotations();
    if (!this.dimensions||!this.presented) return;
    const transforms=new Map([...this.paths].map(([path,part])=>[part.instance_id,this.viewer.nestedGroup.groups[path]?.matrixWorld]));
    const annotations=modelDimensions(this.model).filter(resolved).filter(a=>this.visible.has(a.part)&&this.visible.has(a.end_part||a.part)&&this.visible.has(a.vertex_part||a.part)&&
      (this.scope==='all'||this.scope==='selected'&&(this.selected===a.part||this.selected===a.end_part||this.selected===a.vertex_part)||this.scope==='pinned'&&!!a.saved_id));
    annotations.sort((a,b)=>Number(this.selected===b.part||this.selected===b.end_part||this.selected===b.vertex_part)-Number(this.selected===a.part||this.selected===a.end_part||this.selected===a.vertex_part)||Number(!!b.saved_id)-Number(!!a.saved_id));
    for (const annotation of annotations.slice(0,100)) {
        const placement=this.annotationPlacement(annotation,transforms);if(!placement)continue;
        const {segments,anchor,value,valid}=placement;
        const line=dimensionStroke(segments);
        this.annotationGroup.add(line);
        const leader=dimensionStroke([{start:anchor,end:anchor}]);leader.renderOrder=101;
        this.annotationGroup.add(leader);
        // The first drawn annotation updates labels and leader uniforms using
        // this frame's camera. Uniform suppression keeps every leader in the
        // render list, so revealing a displaced label needs no second frame.
        const before=line.onBeforeRender;line.onBeforeRender=(...args)=>{
          const frame=args[0].info.render.frame;
          if(this.labelRenderFrame!==frame){this.labelRenderFrame=frame;this.projectLabels();}
          before.apply(line,args);
        };
        const element = document.createElement('span'); element.className = 'dimension-label';
        element.textContent = dimensionText({...annotation,value});
        element.title = annotation.parameter ? `Parameter: ${annotation.parameter}` : 'Exact reference dimension';
        // Only the deliberate inspect button receives pointer events. Ordinary
        // label text lets navigation gestures pass through to the canvas.
        const inspect=document.createElement('button');inspect.type='button';inspect.className='dimension-inspect';inspect.textContent='ⓘ';inspect.setAttribute('aria-label',`Inspect ${annotation.label}`);
        inspect.onpointerdown=event=>event.stopPropagation();inspect.onpointerup=event=>event.stopPropagation();element.appendChild(inspect);
        this.overlay.appendChild(element);
        const visual={annotation,line,leader,element,anchor,value};this.visuals.push(visual);
        inspect.onclick=()=>this.onAnnotation({...annotation,value:visual.value});
        this.labels.push({element,anchor,offset:annotation.label_offset||[0,0],leader});
        line.visible=valid;leader.material.uniforms.enabled.value=0;element.style.display=valid?'':'none';
    }
    this.projectLabels();
  }
  private annotationPlacement(annotation:ResolvedAnnotation,transforms:Map<string,THREE.Matrix4|undefined>){
    return placeAnnotation(annotation,transforms,this.localAngleLayouts);
  }
  private updateAnnotationPoses(){
    if(!this.visuals.length)return;
    const transforms=new Map([...this.paths].map(([path,part])=>[part.instance_id,this.viewer.nestedGroup.groups[path]?.matrixWorld]));
    for(const visual of this.visuals){
      const {annotation,line,leader,element,anchor}=visual;
      const placement=this.annotationPlacement(annotation,transforms);if(!placement)continue;
      visual.value=placement.value;
      line.visible=placement.valid;leader.material.uniforms.enabled.value=0;element.style.display=placement.valid?'':'none';
      moveDimensionStroke(line,placement.segments);
      anchor.copy(placement.anchor);
      moveDimensionStroke(leader,[{start:anchor,end:anchor}]);
      if(element.firstChild)element.firstChild.textContent=dimensionText({...annotation,value:visual.value});
    }
    this.projectLabels();
  }
  private projectLabels() {
    if (!this.parts.length) return;
    const canvas = this.viewer.renderer.domElement.getBoundingClientRect();
    const host = this.container.getBoundingClientRect();
    const requests=this.labels.map(({element,anchor,offset})=>{
      const p=anchor.clone().project(this.viewer.camera.getCamera());
      // A perspective point behind the eye can have a finite NDC position.
      const eye=anchor.clone().applyMatrix4(this.viewer.camera.getCamera().matrixWorldInverse);
      return {x:(p.x+1)*canvas.width/2,y:(1-p.y)*canvas.height/2,w:element.offsetWidth||130,h:element.offsetHeight||22,depth:element.style.display==='none'||eye.z>=0?Infinity:p.z,offset};
    });
    const placements=placeDimensionLabels(requests,canvas.width,canvas.height);
    this.labels.forEach(({element,leader},index)=>{
      const box=placements[index],request=requests[index];
      element.style.visibility=box?'visible':'hidden';
      leader.material.uniforms.enabled.value=Number(!!box&&Math.hypot(box.x-request.x,box.y-request.y)>4);
      if(!box)return;
      element.style.transform=`translate(${box.x+canvas.left-host.left}px, ${box.y+canvas.top-host.top}px) translate(-50%, -50%)`;
      leader.material.uniforms.endOffset.value.set(box.x-request.x,request.y-box.y);
    });
  }

  private detachListeners(){
    this.viewer?.renderer?.domElement?.removeEventListener('webglcontextlost',this.graphicsLost);
    this.viewer?.renderer?.domElement?.removeEventListener('webglcontextrestored',this.graphicsRestored);
    this.container.removeEventListener('pointerdown',this.pointerDown,true);
    this.container.removeEventListener('pointerup',this.pointerUp,true);
    this.container.removeEventListener('pointermove',this.pointerMove,true);
    this.container.removeEventListener('pointercancel',this.pointerCancel,true);
    this.container.removeEventListener('pointerleave',this.pointerLeave,true);
    this.container.removeEventListener('wheel',this.pointerLeave,true);
    this.container.removeEventListener('lostpointercapture',this.pointerCancel,true);
  }

  dispose() {
    this.cancelLoad();this.revisionBytes.clear();
    this.renderStorage?.dispose();this.renderStorage=null;
    this.disposed = true;this.presented=false;this.repairCamera=null; this.loadSequence++;this.cancelSimulationFrame();this.cancelHover();
    this.renderedBuffers?.dispose();this.renderedBuffers=null;
    this.clearHighlights();for(const item of this.pickGeometry.values())item.dispose();this.pickGeometry.clear();
    this.resize.disconnect(); this.clearAnnotations();
    this.cancelDrag();this.cancelMarquee();
    this.detachListeners();
    this.hoverStatusElement?.remove();this.hoverStatusElement=null;this.motionStatusElement?.remove();this.motionStatusElement=null;
    this.viewer.dispose(); this.display.dispose(); this.host.remove(); this.overlay.remove();this.badgeLayer.remove();this.marqueeElement.remove();this.pressMarker.remove();
  }
}
