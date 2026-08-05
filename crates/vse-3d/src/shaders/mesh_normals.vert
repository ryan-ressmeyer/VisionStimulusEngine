#version 450

layout(location = 0) in vec3 position;
layout(location = 0) out vec3 world_position;

layout(push_constant) uniform PushConstants {
    mat4 model;
    mat4 view_projection;
} pc;

void main() {
    vec4 world = pc.model * vec4(position, 1.0);
    world_position = world.xyz;
    gl_Position = pc.view_projection * world;
}
