precision highp float;
uniform sampler2D u_tex;
uniform float u_time;
uniform float u_aspect;
uniform vec2 u_resolution;
uniform int u_login_state;
uniform float u_state_time;
varying vec2 v_uv;

void main() {
  vec2 px = 1.0 / u_resolution;
  const float N = 9.0; // (radius+1)^2, radius=2

  vec3 m0 = vec3(0.0), s0 = vec3(0.0);
  vec3 m1 = vec3(0.0), s1 = vec3(0.0);
  vec3 m2 = vec3(0.0), s2 = vec3(0.0);
  vec3 m3 = vec3(0.0), s3 = vec3(0.0);

  for (int j = -2; j <= 0; j++) {
    for (int i = -2; i <= 0; i++) {
      vec3 c = texture2D(u_tex, v_uv + vec2(float(i), float(j)) * px).rgb;
      m0 += c; s0 += c * c;
    }
  }
  for (int j = -2; j <= 0; j++) {
    for (int i = 0; i <= 2; i++) {
      vec3 c = texture2D(u_tex, v_uv + vec2(float(i), float(j)) * px).rgb;
      m1 += c; s1 += c * c;
    }
  }
  for (int j = 0; j <= 2; j++) {
    for (int i = -2; i <= 0; i++) {
      vec3 c = texture2D(u_tex, v_uv + vec2(float(i), float(j)) * px).rgb;
      m2 += c; s2 += c * c;
    }
  }
  for (int j = 0; j <= 2; j++) {
    for (int i = 0; i <= 2; i++) {
      vec3 c = texture2D(u_tex, v_uv + vec2(float(i), float(j)) * px).rgb;
      m3 += c; s3 += c * c;
    }
  }

  m0 /= N; s0 = abs(s0 / N - m0 * m0);
  m1 /= N; s1 = abs(s1 / N - m1 * m1);
  m2 /= N; s2 = abs(s2 / N - m2 * m2);
  m3 /= N; s3 = abs(s3 / N - m3 * m3);

  float v0 = dot(s0, vec3(1.0));
  float v1 = dot(s1, vec3(1.0));
  float v2 = dot(s2, vec3(1.0));
  float v3 = dot(s3, vec3(1.0));

  vec3 col = m0;
  float vmin = v0;
  if (v1 < vmin) { col = m1; vmin = v1; }
  if (v2 < vmin) { col = m2; vmin = v2; }
  if (v3 < vmin) { col = m3; }

  gl_FragColor = vec4(col, 1.0);
}
