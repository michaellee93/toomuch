precision highp float;
varying vec2 v_uv;
uniform float u_time;
uniform float u_aspect;

float smin(float a, float b, float k) {
  float h = max(k - abs(a - b), 0.0) / k;
  return min(a, b) - h * h * k * 0.25;
}

void main() {
  vec2 uv = (v_uv - 0.5) * 2.0;
  uv.x *= u_aspect;
  float t = u_time * 0.22;

  vec2 o0 = vec2(sin(t * 1.00)       * 0.55 * u_aspect, cos(t * 0.70)       * 0.45);
  vec2 o1 = vec2(sin(t * 0.60 + 2.1) * 0.60 * u_aspect, cos(t * 1.20 + 1.0) * 0.40);
  vec2 o2 = vec2(sin(t * 1.30 + 4.2) * 0.40 * u_aspect, cos(t * 0.50 + 3.1) * 0.55);
  vec2 o3 = vec2(sin(t * 0.90 + 1.5) * 0.50 * u_aspect, cos(t * 1.40 + 0.7) * 0.35);

  float d = length(uv - o0) - 0.28;
  d = smin(d, length(uv - o1) - 0.22, 0.35);
  d = smin(d, length(uv - o2) - 0.20, 0.30);
  d = smin(d, length(uv - o3) - 0.18, 0.28);

  float glow = exp(-max(d, 0.0) * 4.5);
  float fill = smoothstep(0.01, -0.01, d);
  float rim  = smoothstep(0.0, 0.07, d) * smoothstep(0.14, 0.03, d);

  float w0 = exp(-length(uv - o0) * 2.5);
  float w1 = exp(-length(uv - o1) * 2.5);
  float w2 = exp(-length(uv - o2) * 2.5);
  float w3 = exp(-length(uv - o3) * 2.5);
  float wt = w0 + w1 + w2 + w3 + 1e-5;

  vec3 c0 = vec3(0.14, 0.04, 0.40);
  vec3 c1 = vec3(0.04, 0.10, 0.50);
  vec3 c2 = vec3(0.30, 0.04, 0.35);
  vec3 c3 = vec3(0.04, 0.16, 0.45);
  vec3 orbColor = (c0*w0 + c1*w1 + c2*w2 + c3*w3) / wt;

  vec3 col = vec3(0.03, 0.03, 0.055);
  col += orbColor * glow * 0.55;
  col += orbColor * fill * 0.35;
  col += vec3(0.55, 0.65, 1.0) * rim * 0.12;
  col *= 1.0 - dot(v_uv - 0.5, v_uv - 0.5) * 1.1;

  gl_FragColor = vec4(col, 1.0);
}
