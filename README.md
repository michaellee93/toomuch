# toomuch
This project is a Linux greeter frontend for greetd that uses DRM/KMS and EGL to run shaders to create dynamic login screens.

There is a [playground](https://michaellee93.github.io/toomuch/) which allows you to test your shaders in the browser.

It is an experimental render toy: native DRM/EGL and wasm/WebGL share one Rust
rendering core.

## Status

The useful core is in place:

- login state machine and `LoginView`
- native DRM/EGL display path
- native keyboard input path
- wasm/WebGL entry point
- config-driven background shader, text layer, optional background effect, and
  post-process shader
- web shader editor proof of concept

The current render order is:

```text
background -> optional background effect -> text -> postproc -> screen
```

This works, but the effect/postproc split is still too specific. Both should
probably become generic fullscreen shader passes with named texture inputs.

## Run

Native development:

```sh
cargo run --bin toomuch
```

The binary reads `/etc/toomuch/config.toml` and falls back to built-in defaults
if it is missing.

Web build:

```sh
wasm-pack build --release --target web --out-dir web/pkg
```

Then serve `web/` with any static file server.

## Config

Example:

```toml
[input]
font_size = 104
username_prefix = "Hello "
password_prefix = ""
cursor = "_"

[background]
kind = "shader"
path = "/etc/toomuch/protean.glsl"

[background.effect]
path = "/etc/toomuch/ascii.glsl"

[post]
path = "/etc/toomuch/post.glsl"
```

Shaders are GLSL ES fragment shaders. They are not raw ShaderToy shaders unless
adapted to this interface.

Common uniforms:

```glsl
uniform sampler2D u_tex;
uniform float u_time;
uniform float u_aspect;
uniform vec2 u_resolution;
uniform int u_login_state;
uniform float u_state_time;
```

Background and effect shaders also receive `varying vec2 v_uv`. Background
effects may use `uniform sampler2D u_aux_tex`.

## Missing Real Features

- still image decode / texture source
- native video stream / animated texture source
- named texture inputs for effects
- polished input renderers
- real `floating` input renderer
- cleaner generic pass pipeline

Editor features such as CodeMirror and better shader errors are useful, but they
are not core engine work.

## Checks

Known-good commands:

```sh
cargo test
cargo build --bin toomuch
cargo check --target wasm32-unknown-unknown --lib
wasm-pack build --release --target web --out-dir web/pkg
```
