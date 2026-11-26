/** Export formats that describe the same part and should share one mention row. */
export const PART_FORMATS:Record<string,string>={stl:'STL',step:'STEP',stp:'STEP'};
/** Display order for a grouped part's formats, regardless of file sort order. */
export const PART_FORMAT_ORDER=['STL','STEP'];

export type PartFile={directory:string;stem:string;extension:string;format:string};

/** Split `parts/bracket.step` into its directory, stem and recognized format. */
export function parsePartFile(path:string):PartFile|null{
  const slash=path.lastIndexOf('/');
  const name=path.slice(slash+1);
  const dot=name.lastIndexOf('.');
  if(dot<=0)return null;
  const extension=name.slice(dot+1).toLocaleLowerCase();
  const format=PART_FORMATS[extension];
  return format?{directory:path.slice(0,slash+1),stem:name.slice(0,dot),extension,format}:null;
}

/** Stable group identity: same folder and stem, case-insensitive on the stem. */
export function partFileKey(path:string){
  const parsed=parsePartFile(path);
  return parsed?`${parsed.directory}${parsed.stem.toLocaleLowerCase()}`:null;
}

/** Tie-break when one row represents several formats. */
export function partFileRank(path:string){
  const parsed=parsePartFile(path);
  return parsed?.extension==='stl'?0:parsed?.extension==='step'?1:parsed?.extension==='stp'?2:3;
}
