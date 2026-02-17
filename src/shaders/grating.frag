#version 460

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;
    float frequency;
    float orientation;
    float phase;
    float contrast;
    float background;
    float sigma;
    uint wave_type;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    // Map UV to pixel offset from rect center
    vec2 rect_size = vec2(pc.rect.z - pc.rect.x, pc.rect.w - pc.rect.y);
    vec2 pixel = (v_uv - 0.5) * rect_size;

    // Rotate to grating orientation
    float cos_ori = cos(pc.orientation);
    float sin_ori = sin(pc.orientation);
    float x_rot = pixel.x * cos_ori + pixel.y * sin_ori;

    // Carrier
    float carrier = sin(6.2831853 * pc.frequency * x_rot + pc.phase);

    // Square wave: threshold the sine
    if (pc.wave_type == 1u) {
        carrier = carrier >= 0.0 ? 1.0 : -1.0;
    }

    float luminance = pc.background + pc.contrast * 0.5 * carrier;
    luminance = clamp(luminance, 0.0, 1.0);

    f_color = vec4(luminance, luminance, luminance, 1.0);
}
