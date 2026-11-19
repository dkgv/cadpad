import * as THREE from 'three';

/** Temporary canvas for raw triangles while the committed model is prepared. */
export function stlPreviewViewport(container:HTMLElement,onFrame:()=>void){
  const scene=new THREE.Scene();
  const camera=new THREE.OrthographicCamera();
  const material=new THREE.MeshStandardMaterial({color:0xb7c8d2,roughness:0.65,flatShading:true,side:THREE.DoubleSide});
  const light=new THREE.DirectionalLight(0xffffff,2.5);
  light.position.set(1,-2,3);
  scene.add(light,new THREE.AmbientLight(0xffffff,1.4));
  const bounds=new THREE.Box3();
  const meshes:THREE.Mesh[]=[];
  let renderer:THREE.WebGLRenderer|undefined;
  let frame:number|undefined,paint:number|undefined;
  let unavailable=false,shown=false;
  function draw(){
    frame=undefined;
    const width=container.clientWidth,height=container.clientHeight;
    if(!renderer||width<1||height<1||bounds.isEmpty())return;
    const sphere=bounds.getBoundingSphere(new THREE.Sphere());
    const radius=Math.max(sphere.radius,0.001);
    const aspect=width/height;
    const halfHeight=1.08*radius/Math.min(1,aspect);
    camera.left=-halfHeight*aspect;camera.right=halfHeight*aspect;
    camera.top=halfHeight;camera.bottom=-halfHeight;
    camera.near=radius*0.01;camera.far=radius*6;
    camera.up.set(0,0,1);
    camera.position.copy(sphere.center).addScaledVector(direction,radius*3);
    camera.lookAt(sphere.center);
    camera.updateProjectionMatrix();
    if(renderer.domElement.clientWidth!==width||renderer.domElement.clientHeight!==height){
      renderer.setSize(width,height);
    }
    renderer.render(scene,camera);
    if(!shown){
      shown=true;
      container.style.visibility='visible';
      paint=requestAnimationFrame(()=>{if(!document.hidden)onFrame();});
    }
  }
  const direction=new THREE.Vector3(1,-1,1).normalize();
  const schedule=()=>{if(frame===undefined)frame=requestAnimationFrame(draw);};
  const resize=typeof ResizeObserver==='undefined'?undefined:new ResizeObserver(schedule);
  resize?.observe(container);
  function add(chunk:ArrayBuffer){
    if(unavailable)return;
    if(!renderer){
      try{
        renderer=new THREE.WebGLRenderer({alpha:true,antialias:true});
        container.appendChild(renderer.domElement);
      }catch{unavailable=true;return;}
    }
    const geometry=new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(new Float32Array(chunk),3));
    geometry.computeBoundingBox();
    bounds.union(geometry.boundingBox!);
    const mesh=new THREE.Mesh(geometry,material);
    mesh.frustumCulled=false;
    meshes.push(mesh);
    scene.add(mesh);
    schedule();
  }
  function dispose(){
    resize?.disconnect();
    if(frame!==undefined)cancelAnimationFrame(frame);
    if(paint!==undefined)cancelAnimationFrame(paint);
    container.style.visibility='hidden';
    for(const mesh of meshes)mesh.geometry.dispose();
    material.dispose();
    renderer?.dispose();renderer?.forceContextLoss();renderer?.domElement.remove();
  }
  return {add,dispose};
}
