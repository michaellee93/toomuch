// http://www.pouet.net/prod.php?which=57245
// Original by Danilo Guanabara. Adapted for toomuch.

precision highp float;
varying vec2 v_uv;
uniform float u_time;
uniform vec2 u_resolution;

void main() {
    vec2 fragCoord = v_uv * u_resolution;
    vec3 c;
    float l, z = u_time;
    for (int i = 0; i < 3; i++) {
        vec2 uv, p = fragCoord / u_resolution;
        uv = p;
        p -= 0.5;
        p.x *= u_resolution.x / u_resolution.y;
        z += 0.07;
        l = length(p);
        uv += p / l * (sin(z) + 1.0) * abs(sin(l * 9.0 - z - z));
        c[i] = 0.01 / length(mod(uv, 1.0) - 0.5);
    }
    gl_FragColor = vec4(c / l, 1.0);
}
