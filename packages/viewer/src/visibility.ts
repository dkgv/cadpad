import type {Viewer} from 'three-cad-viewer';

/** Object IDs and navigation-tree name paths are separate in the viewer. The
 * renderer API accepts our stable object ID; setState only accepts tree names. */
export function setRenderedVisibility(viewer:Pick<Viewer,'setObject'>,path:string,visible:boolean){
  const state=visible?1:0;
  viewer.setObject(path,state,0,false,false);
  viewer.setObject(path,state,1,false,false);
}
