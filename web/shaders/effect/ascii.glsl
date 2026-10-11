precision highp float;
uniform sampler2D u_tex;
uniform sampler2D u_aux_tex;
uniform float u_time;
uniform float u_aspect;
uniform vec2 u_resolution;
uniform int u_login_state;
uniform float u_state_time;
varying vec2 v_uv;

const float CHAR_W   = 16.0;
const float CHAR_H   = 16.0;
const float RAMP_LEN = 95.0;

void main() {
  vec2 cSz   = vec2(CHAR_W, CHAR_H);
  vec2 cidx  = floor(v_uv * u_resolution / cSz);
  vec2 ctr   = (cidx + 0.5) * cSz / u_resolution;

  vec4 bg    = texture2D(u_tex, ctr);
  float luma  = dot(bg.rgb, vec3(0.299, 0.587, 0.114));
  float lumag = pow(luma, 0.45);

  float ci    = floor(lumag * (RAMP_LEN - 1.0));
  vec2 inCell = fract(v_uv * u_resolution / cSz);
  float mask  = texture2D(u_aux_tex, vec2((ci + inCell.x) / RAMP_LEN, 1.0 - inCell.y)).r;

  vec3 bloom = vec3(0.0);
  float bwt  = 0.0;
  for (int dy = -2; dy <= 2; dy++) {
    for (int dx = -2; dx <= 2; dx++) {
      vec2 nc  = (cidx + vec2(float(dx), float(dy)) + 0.5) * cSz / u_resolution;
      vec3 s   = texture2D(u_tex, clamp(nc, vec2(0.0), vec2(1.0))).rgb;
      float nl = dot(s, vec3(0.299, 0.587, 0.114));
      float w  = exp(-float(dx * dx + dy * dy) * 0.3) * nl * nl;
      bloom   += s * w;
      bwt     += w;
    }
  }
  if (bwt > 0.0) bloom /= bwt;

  vec3 char_col = bg.rgb * mask;
  vec3 glow_col = bloom * (1.0 - mask) * lumag * 0.7;
  vec3 dark_tint = bg.rgb * (1.0 - mask) * 0.12;
  gl_FragColor = vec4(char_col + glow_col + dark_tint, 1.0);
}
