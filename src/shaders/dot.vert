#version 460

// Per-vertex: unit quad [-1, 1]
layout(location = 0) in vec2 quad_pos;

// Per-instance: dot center in pixel coords
layout(location = 1) in vec2 instance_pos;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    float dot_radius;
    float _pad;
    vec4 dot_color;
} pc;

layout(location = 0) out vec2 v_local;  // [-1, 1] within dot quad
layout(location = 1) out vec4 v_color;

void main() {
    // Scale unit quad to dot radius and offset to dot center
    vec2 pixel_pos = instance_pos + quad_pos * pc.dot_radius;
    vec2 ndc = (pixel_pos / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_local = quad_pos;
    v_color = pc.dot_color;
}
