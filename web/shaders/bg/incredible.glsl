precision highp float;
varying vec2 v_uv;
uniform float u_time;
uniform float u_aspect;
uniform float u_resolution;


void main(){
	vec3 c;
	float l,z=u_time;
	for(int i=0;i<3;i++) {
		vec2 uv,p=uv.xy/u_rresolution;
		uv=p;
		p-=.5;
		p.x*=u_resolution.x/u_resolution.y;
		z+=.07;
		l=length(p);
		uv+=p/l*(sin(z)+1.)*abs(sin(l*9.-z-z));
		c[i]=.01/length(mod(uv,1.)-.5);
	}
	gl_FragColor=vec4(c/l,t);
}
