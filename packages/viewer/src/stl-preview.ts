import * as THREE from 'three';

export function showStlPreview(container: HTMLElement, bytes: ArrayBuffer): () => void {
  if (!bytes.byteLength || bytes.byteLength % 36 !== 0) throw new Error('Invalid preview mesh');
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(new Float32Array(bytes), 3));
  geometry.computeBoundingSphere();
  const sphere = geometry.boundingSphere!;
  geometry.translate(-sphere.center.x, -sphere.center.y, -sphere.center.z);
  const radius = sphere.radius;
  if (!(radius > 0 && Number.isFinite(radius))) {
    geometry.dispose();
    throw new Error('Invalid preview bounds');
  }

  let renderer: THREE.WebGLRenderer;
  try {
    renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
  } catch (error) {
    geometry.dispose();
    throw error;
  }
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  container.appendChild(renderer.domElement);

  const material = new THREE.MeshNormalMaterial({ flatShading: true, side: THREE.DoubleSide });
  const scene = new THREE.Scene();
  scene.add(new THREE.Mesh(geometry, material));
  const camera = new THREE.OrthographicCamera(-radius, radius, radius, -radius, 0, radius * 6);
  camera.up.set(0, 0, 1);
  camera.position.set(1, -1.5, 1).normalize().multiplyScalar(radius * 3);
  camera.lookAt(0, 0, 0);

  const resize = new ResizeObserver(() => {
    const { clientWidth: width, clientHeight: height } = container;
    if (!width || !height) return;
    const aspect = width / height;
    const halfWidth = radius * 1.1 * Math.max(1, aspect);
    const halfHeight = radius * 1.1 * Math.max(1, 1 / aspect);
    camera.left = -halfWidth;
    camera.right = halfWidth;
    camera.top = halfHeight;
    camera.bottom = -halfHeight;
    camera.updateProjectionMatrix();
    renderer.setSize(width, height);
    renderer.render(scene, camera);
  });
  resize.observe(container);

  return () => {
    resize.disconnect();
    geometry.dispose();
    material.dispose();
    renderer.dispose();
    renderer.domElement.remove();
  };
}
