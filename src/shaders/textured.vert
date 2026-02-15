#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
} pc;

layout(location = 0) out vec2 v_uv;

void main() {
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = uv;
}
