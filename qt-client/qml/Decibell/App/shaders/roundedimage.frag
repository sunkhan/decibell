#version 440
// A picture clipped to a rounded rectangle, anti-aliased by its signed distance: avatars (squircles), attachment
// thumbnails, the server tile. No offscreen layer: the Image item is the texture source.
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 itemSize;   // px
    float radius;    // px
    vec4 crop;       // texture rect shown: x, y, w, h (0..1), for aspect crop
};
layout(binding = 1) uniform sampler2D source;

void main() {
    vec2 p = qt_TexCoord0 * itemSize;
    vec2 q = abs(p - itemSize * 0.5) - (itemSize * 0.5 - vec2(radius));
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    float a = clamp(0.5 - d, 0.0, 1.0);
    fragColor = texture(source, crop.xy + qt_TexCoord0 * crop.zw) * (a * qt_Opacity);
}
