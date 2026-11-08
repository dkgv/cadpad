import type {Inspection} from '@cadpad/viewer';

export type IconName='chat'|'search'|'cube'|'viewIso'|'viewFront'|'viewTop'|'viewRight'|'sparkles'|'measure'|'motion'|'settings'|'undo'|'export'|'save'|'eye'|'eyeOff'|'fit'|'chevron'|'close'|'arrowUp'|'plus'|'sliders'|'recordings'|'tests'|'folder'|'file'|'import'|'physics'|'history'|'edit'|'splitRight'|'splitDown';
const cameraCube=['M12 2.5 22 8v9L12 22.5 2 17V8Z','M2 8l10 5.5L22 8','M12 13.5v9'];
const cameraFaces:Partial<Record<IconName,string>>={
  viewIso:'M12 2.5 22 8v9L12 22.5 2 17V8Z',
  viewFront:'M2 8l10 5.5v9L2 17Z',
  viewTop:'M12 2.5 22 8l-10 5.5L2 8Z',
  viewRight:'M12 13.5 22 8v9l-10 5.5Z'
};
const paths:Record<IconName,string[]>={
  chat:['M21 15a3 3 0 0 1-3 3H8l-5 4V6a3 3 0 0 1 3-3h12a3 3 0 0 1 3 3Z'],
  search:['M21 21l-5-5','M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0'],
  splitRight:['M3 4h18v16H3Z','M12 4v16'],
  splitDown:['M3 4h18v16H3Z','M3 12h18'],
  cube:['M12 3 3 8v9l9 5 9-5V8Z','m3 8 9 5 9-5','M12 13v9','m7.5 5.5 9 5'],
  // A consistent cube silhouette, with the requested viewing face highlighted.
  viewIso:cameraCube,viewFront:cameraCube,viewTop:cameraCube,viewRight:cameraCube,
  sparkles:['m12 3 2.3 6.7L21 12l-6.7 2.3L12 21l-2.3-6.7L3 12l6.7-2.3Z','m20 2 .6 1.4L22 4l-1.4.6L20 6l-.6-1.4L18 4l1.4-.6Z'],
  measure:['m4 16 12-12 4 4L8 20Z','m8 12 2 2','m12 8 2 2','m6 14 2 2'],
  motion:['M2 21h6','M5 19v2','M7 17a2 2 0 1 0-4 0 2 2 0 0 0 4 0','M14 6a2 2 0 1 0-4 0 2 2 0 0 0 4 0','M21 11a2 2 0 1 0-4 0 2 2 0 0 0 4 0','m6.1 15.3 4.8-7.6','m13.7 7.2 3.6 2.6','M19 13v4m-2 0h4'],
  settings:['M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8','m10 3-.5 2-2 1L5.5 5 3 9l1.5 1.5v3L3 15l2.5 4 2-1 2 1 .5 2h5l.5-2 2-1 2 1 2.5-4-1.5-1.5v-3L22 9l-2.5-4-2 1-2-1L15 3Z'],
  undo:['M4 9h10a6 6 0 0 1 0 12h-3','m8 5-4 4 4 4'],
  folder:['M3 7V4h6l2 3h10v13H3Z'],
  file:['M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9Z','M14 3v6h6'],
  import:['M12 3v12','m8 11 4 4 4-4','M5 15v6h14v-6'],
  export:['M12 15V3','m8 7 4-4 4 4','M5 13v7h14v-7'],
  save:['M5 3h12l4 4v14H3V3Z','M7 3v6h10V3','M7 21v-8h10v8'],
  eye:['M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z','M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6'],
  eyeOff:['m3 3 18 18','M10 5c7-2 12 7 12 7a19 19 0 0 1-4 4','M6 6a21 21 0 0 0-4 6s4 7 10 7a12 12 0 0 0 5-1','m10 10 4 4'],
  fit:['M8 3H3v5','M16 3h5v5','M21 16v5h-5','M8 21H3v-5'],
  chevron:['m8 5 7 7-7 7'],close:['m6 6 12 12','M18 6 6 18'],arrowUp:['M12 19V5','m6 11 6-6 6 6'],plus:['M12 5v14','M5 12h14'],
  recordings:['M4 4h16v16H4Z','M4 8h16','M4 16h16','M8 4v4m8-4v4M8 16v4m8-4v4','m10 10 4 2-4 2Z'],
  edit:['m15 4 5 5','M4 16 16 4a2.1 2.1 0 0 1 3 3L7 19l-4 1Z'],
  history:['M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.7 2.7L3 8','M3 3v5h5','M12 7v5l3 2'],
  physics:['M10 7a3 3 0 1 0-6 0 3 3 0 0 0 6 0','M17 3v13','m13 12 4 4 4-4','M3 21h18'],
  tests:['M9 5H6a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-3','M9 2h6v5H9Z','m8 14 3 3 5-6'],
  sliders:['M4 6h5m4 0h7','M4 12h9m4 0h3','M4 18h2m4 0h10','M9 3v6','M13 9v6','M6 15v6']
};
export function Icon({name,size=18}:{name:IconName;size?:number}){
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.65" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{cameraFaces[name]&&<path d={cameraFaces[name]} fill="currentColor" fillOpacity={name==='viewIso'?0.1:0.24} stroke="none"/>}{paths[name].map((d,i)=><path key={i} d={d}/>)}</svg>;
}
export function humanLabel(value:string){return value.replaceAll('_',' ').replace(/^./,s=>s.toUpperCase());}
export function errorSummary(value:string){
  try{const data=JSON.parse(value);const message=data?.message||data?.error?.message;return typeof message==='string'?message:'This action could not be completed.';}catch{return value;}
}
export function PropertySummary({inspection}:{inspection:Inspection|null}){
  if(!inspection)return <p className="muted">Reading exact geometry…</p>;
  const entity=(inspection.entities?.[0]||{}) as unknown as Record<string,unknown>;
  const fields=[['volume','Volume','mm³'],['area','Surface area','mm²'],['length','Length','mm'],['radius','Radius','mm']] as const;
  const bounds=entity.bounds as {size?:number[]}|undefined;
  const type=entity.surface_type||entity.curve_type||entity.kind;
  const format=(n:number)=>n.toLocaleString(undefined,{maximumFractionDigits:4});
  return <><dl className="property-grid">
    {typeof type==='string'&&<><dt>Geometry</dt><dd>{humanLabel(type.toLowerCase())}</dd></>}
    {bounds?.size?.map((value,index)=><div className="property-pair" key={index}><dt>{['Width','Depth','Height'][index]}</dt><dd>{format(value)} <small>mm</small></dd></div>)}
    {fields.map(([key,label,unit])=>typeof entity[key]==='number'&&<div className="property-pair" key={key}><dt>{label}</dt><dd>{format(entity[key])} <small>{unit}</small></dd></div>)}
  </dl><details className="technical-details"><summary>All geometry properties</summary><pre className="properties">{JSON.stringify(inspection,null,2)}</pre></details></>;
}
