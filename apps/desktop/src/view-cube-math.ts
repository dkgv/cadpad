/**
 * Invert the Z-up camera quaternion into a CSS matrix3d.
 * Conjugates with scaleY(-1) so Three.js Y-up matrix math matches CSS Y-down.
 */
export function cameraToCssMatrix(qx:number,qy:number,qz:number,qw:number){
  const x=-qx,y=-qy,z=-qz,w=qw;
  const x2=x+x,y2=y+y,z2=z+z;
  const xx=x*x2,xy=x*y2,xz=x*z2;
  const yy=y*y2,yz=y*z2,zz=z*z2;
  const wx=w*x2,wy=w*y2,wz=w*z2;
  const m00=1-(yy+zz),m01=xy+wz,m02=xz-wy;
  const m10=xy-wz,m11=1-(xx+zz),m12=yz+wx;
  const m20=xz+wy,m21=yz-wx,m22=1-(xx+yy);
  return `matrix3d(${m00},${-m10},${m20},0,${-m01},${m11},${-m21},0,${m02},${-m12},${m22},0,0,0,0,1)`;
}
