#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;          // left, top, right, bottom in pixels
    float frequency;
    float orientation;
    float phase;
    float contrast;
    float background;
    float sigma;        // used by gabor, ignored by grating
    float aspect_ratio; // Gaussian width-to-height ratio
    uint wave_type;     // 0=sine, 1=square
    uint composite_mode; // 0=opaque, 1=positive add, 2=negative subtract
} pc;

layout(location = 0) out vec2 v_uv;

void main() {
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = uv;
}
