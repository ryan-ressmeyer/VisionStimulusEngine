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
    float aspect_ratio;
    uint wave_type;
    uint composite_mode;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    vec2 rect_size = vec2(pc.rect.z - pc.rect.x, pc.rect.w - pc.rect.y);
    vec2 pixel = (v_uv - 0.5) * rect_size;

    float cos_ori = cos(pc.orientation);
    float sin_ori = sin(pc.orientation);
    float x_rot = pixel.x * cos_ori + pixel.y * sin_ori;
    float y_rot = -pixel.x * sin_ori + pixel.y * cos_ori;

    // Gaussian envelope. This matches Psychtoolbox's nonsymmetric Gabor:
    // aspect_ratio > 1 narrows the y' axis.
    float gamma_y = pc.aspect_ratio * y_rot;
    float gaussian = exp(-(x_rot * x_rot + gamma_y * gamma_y) / (2.0 * pc.sigma * pc.sigma));

    // Carrier
    float carrier = sin(6.2831853 * pc.frequency * x_rot + pc.phase);
    if (pc.wave_type == 1u) {
        carrier = carrier >= 0.0 ? 1.0 : -1.0;
    }

    float modulation = pc.contrast * 0.5 * gaussian * carrier;
    if (pc.composite_mode == 1u) {
        // Positive half for the ONE+ONE accumulation pass.
        float positive = max(modulation, 0.0);
        f_color = vec4(positive, positive, positive, 0.0);
    } else if (pc.composite_mode == 2u) {
        // Negative magnitude for the destination-minus-source pass.
        float negative = max(-modulation, 0.0);
        f_color = vec4(negative, negative, negative, 0.0);
    } else {
        float luminance = clamp(pc.background + modulation, 0.0, 1.0);
        f_color = vec4(luminance, luminance, luminance, 1.0);
    }
}
