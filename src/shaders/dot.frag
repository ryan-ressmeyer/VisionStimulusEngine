#version 460

layout(location = 0) in vec2 v_local;
layout(location = 1) in vec4 v_color;
layout(location = 0) out vec4 f_color;

void main() {
    // Circular dot with anti-aliased edge
    float dist = length(v_local);
    if (dist > 1.0) {
        discard;
    }
    // Smooth edge over last 5% of radius
    float alpha = 1.0 - smoothstep(0.95, 1.0, dist);
    f_color = vec4(v_color.rgb, v_color.a * alpha);
}
