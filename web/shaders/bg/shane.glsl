// Original by Shane (Shadertoy). Adapted for toomuch.
// Comments below are by Shane, thanks!

precision highp float;
varying vec2 v_uv;
uniform float u_time;
uniform vec2 u_resolution;

int hexid;
vec3 hpos, point, pt;
float tcol, bcol, hitbol, hexpos, fparam=0.;

mat2 rot(float a) {
    float s=sin(a),c=cos(a);
    return mat2(c,s,-s,c);
}

vec3 path(float t) {
    return vec3(sin(t*.3+cos(t*.2)*.5)*4.,cos(t*.2)*3.,t);
}

float hexagon( in vec2 p, in float r )
{
    const vec3 k = vec3(-0.866025404,0.5,0.577350269);
    p = abs(p);
    p -= 2.0*min(dot(k.xy,p),0.0)*k.xy;
    p -= vec2(clamp(p.x, -k.z*r, k.z*r), r);
    return length(p)*sign(p.y);
}

float hex(vec2 p) {
    p.x *= 0.57735*2.0;
    p.y+=mod(floor(p.x),2.0)*0.5;
    p=abs((mod(p,1.0)-0.5));
    return abs(max(p.x*1.5 + p.y, p.y*2.0) - 1.0);
}

mat3 lookat(vec3 dir) {
    vec3 up=vec3(0.,1.,0.);
    vec3 rt=normalize(cross(dir,up));
    return mat3(rt, cross(rt,dir), dir);
}

float hash12(vec2 p)
{
    p*=1000.;
    vec3 p3  = fract(vec3(p.xyx) * .1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Renders a line between points A and B on a sphere with no capping.
float sphereLineAB(vec3 p, vec3 a, vec3 b, float rad){
    p = normalize(p);
    return dot(p, cross(a, b))/length(a - b);
}

// Rotate on axis.
// Blackle https://suricrasia.online/demoscene/functions/
// Point to rotate, point to rotate around, and rotation angle.
vec3 erot(vec3 p, vec3 ax, float ro) {
  return mix(dot(ax, p)*ax, p, cos(ro)) + sin(ro)*cross(ax, p);
}

#define PI 3.14159265359
#define PHI (1. + sqrt(5.))/2.
// Closest icosahedron face center, and face verts. Just a slight rewriting
// of one of TdHooper's function -- Actually, he was kind enough to write this
// one for me. Obviously, it's more complicated than selecting cube faces, but
// it involves the same process, which is stepping on either side of planes.
void icosahedronVerts(in vec3 p, out vec3 face, out vec3 a, out vec3 b, out vec3 c) {

    // Icosaheral Vertices.
    vec3 V = vec3(PHI, 1,  0);
    vec3 ap = abs(p), v = V;
    if (dot(ap, V.yzx - v) > 0.) v = V.yzx;
    if (dot(ap, V.zxy - v) > 0.) v = V.zxy;
    a = normalize(v)*sign(p);

    // Dodecahedron vertices. Icosahedrons and dodecahedrons are duals, so
    // the dodecahedral vertices align to the center of the icosahedron cells,
    // which is pretty handy, since we need the cell center. :)
    v = V.xxx;
    V = vec3(V.zy, V.x + V.y);
    if (dot(ap, V - v) > 0.) v = V;
    if (dot(ap, V.yzx - v) > 0.) v = V.yzx;
    if (dot(ap, V.zxy - v) > 0.) v = V.zxy;
    face = normalize(v)*sign(p);

    float r = PI*2./3.;

    // Rotating the icosahedron vertex around the cell center by TAU/3 will
    // give you one of the other vertices, and rotating the other way produces
    // the other. Obvious... once someone provides the answer. :D
    b = erot(a, face, -r);
    c = erot(a, face, r);
}

float de(vec3 p) {
    pt=vec3(p.xy-path(p.z).xy,p.z);
    float h=abs(hexagon(pt.xy,3.+fparam));
    hexpos=hex(pt.yz);
    tcol=smoothstep(.0,.15,hexpos);
    h-=tcol*.1;
    vec3 pp=p-hpos;
    pp=lookat(point)*pp;
    pp.y-=abs(sin(u_time))*3.+(fparam-(2.-fparam));
    pp.yz*=rot(-u_time);

    float bola=length(pp)-1.;

    // This is a hack to gain some extra speed. The idea being that
    // we shouldn't attempt to map the sphere, if we're not close
    // enough to it. I don't quite trust it, but it seems to work.
    if(length(pp)<1.5){

        // Icosahedron face and vertices.
        vec3 face;
        vec3 va, vb, vc;
        icosahedronVerts(pp, face, va, vb, vc);
        float rad = 1.;

        // Two points on either side of each triangle edge midpoint. There
        // are six in all. Connecting all six lines will form a hexagon in
        // the middle of the icosahedral triangle.
        vec3 mid[6];
        mid[0] = mix(va, vb, 1./3.);
        mid[1] = mix(va, vb, 2./3.);
        mid[2] = mix(vb, vc, 1./3.);
        mid[3] = mix(vb, vc, 2./3.);
        mid[4] = mix(vc, va, 1./3.);
        mid[5] = mix(vc, va, 2./3.);

        // Constructing a hexagon in the triangle face center.
        float poly = -1e5;
        poly = max(poly, sphereLineAB(pp, mid[0], mid[1], rad));
        poly = max(poly, sphereLineAB(pp, mid[1], mid[2], rad));
        poly = max(poly, sphereLineAB(pp, mid[2], mid[3], rad));
        poly = max(poly, sphereLineAB(pp, mid[3], mid[4], rad));
        poly = max(poly, sphereLineAB(pp, mid[4], mid[5], rad));
        poly = max(poly, sphereLineAB(pp, mid[5], mid[0], rad));
        bcol = smoothstep(0., .05, abs(poly));
        bola += bcol*.1;

    }
    vec3 pr=p;
    pr.z=mod(p.z,6.)-3.;
    float d=min(h,bola);
    if (d==bola) {
        tcol=1.;
        hitbol=1.;
    }
    else {
        hitbol=0.;
        bcol=1.;
    }
    return d*.5;
}

vec3 normal(vec3 p) {
    vec2 e=vec2(0.,.005);
    return normalize(vec3(de(p+e.yxx),de(p+e.xyx),de(p+e.xxy))-de(p));
}

vec3 march(vec3 from, vec3 dir) {
    vec3 odir=dir;
    vec3 p=from,col=vec3(0.);
    float d,td=0.;
    vec3 g=vec3(0.);
    for (int i=0; i<200; i++) {
        d=de(p);
        if (d<.001||td>200.) break;
        p+=dir*d;
        td+=d;
        g+=.1/(.1+d)*hitbol*abs(normalize(point));
    }
    float hp=hexpos*(1.-hitbol);
    p-=dir*.01;
    vec3 n=normal(p);
    if (d<.001) {
        col=pow(max(0.,dot(-dir,n)),2.)*vec3(.6,.7,.8)*tcol*bcol;
    }
    col+=float(hexid);
    vec3 pr=pt;
    dir=reflect(dir,n);
    td=0.;
    for (int i=0; i<200; i++) {
        d=de(p);
        if (d<.001||td>200.) break;
        p+=dir*d;
        td+=d;
        g+=.1/(.1+d)*abs(normalize(point));
    }
    float zz=p.z;
    if (d<.001) {
        vec3 refcol=pow(max(0.,dot(-odir,n)),2.)*vec3(.6,.7,.8)*tcol*bcol;
        p=pr;
        p=abs(.5-fract(p*.1));
        float m=100.;
        for (int i=0; i<10; i++) {
            p=abs(p)/dot(p,p)-.8;
            m=min(m,length(p));
        }
        col=mix(col,refcol,m)-m*.3;
        col+=step(.3,hp)*step(.9,fract(pr.z*.05+u_time*.5+hp*.1))*.7;
        col+=step(.3,hexpos)*step(.9,fract(zz*.05+u_time+hexpos*.1))*.3;
    }
    col+=g*.03;
    col.rb*=rot(odir.y*.5);
    return col;
}

void main() {
    vec2 fragCoord = v_uv * u_resolution;
    vec2 uv = fragCoord / u_resolution - 0.5;
    uv.x *= u_resolution.x / u_resolution.y;
    float t = u_time * 2.;
    vec3 from = path(t);
    if (mod(u_time - 10., 20.) > 10.) {
        from = path(floor(t / 20.) * 20. + 10.);
        from.x += 2.;
    }
    hpos = path(t + 3.);
    vec3 adv = path(t + 2.);
    vec3 dir = normalize(vec3(uv, .7));
    vec3 dd = normalize(adv - from);
    point = normalize(adv - hpos);
    point.xz *= rot(sin(u_time) * .2);
    dir = lookat(dd) * dir;
    vec3 col = march(from, dir);
    col *= vec3(1., .9, .8);
    gl_FragColor = vec4(col, 1.0);
}
