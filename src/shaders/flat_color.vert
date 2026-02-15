#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec4 color;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
} pc;

layout(location = 0) out vec4 v_color;

void main() {
    // Transform pixel coordinates to Vulkan NDC [-1, 1]
    // Pixel (0,0) = top-left -> NDC (-1,-1) = top-left in Vulkan
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_color = color;
}
