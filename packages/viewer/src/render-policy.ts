/** three-cad-viewer 5.1.3 has an always-on GPU hover picker with no public switch.
 * Cadpad owns topology hover/selection. Gate the vendor's picker at its shared
 * predicate so it neither reads GPU pixels nor schedules redundant full paints.
 * Keep controls, clipping tools, and explicit selection/camera actions intact.
 * The pinned vendor contract is exercised by render-resources.test.mjs.
 */
export function useCadpadHover(viewer:unknown){
  const controller=(viewer as {pickingController?:{hoverPreselectActive:()=>boolean}}).pickingController;
  if(controller)controller.hoverPreselectActive=()=>false;
}
