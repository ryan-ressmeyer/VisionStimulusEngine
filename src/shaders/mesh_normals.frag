#version 450

layout(location = 0) in vec3 world_position;
layout(location = 0) out vec4 out_color;

void main() {
    vec3 dx = dFdx(world_position);
    vec3 dy = dFdy(world_position);
    // Vulkan fragment coordinates increase downward. Reverse the derivative
    // order so a source CCW +Z face remains +Z after projection-Y correction.
    vec3 normal = normalize(cross(dy, dx));
    out_color = vec4(normal * 0.5 + 0.5, 1.0);
}
