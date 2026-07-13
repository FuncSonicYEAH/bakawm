precision highp float;

uniform float u_alpha;
uniform float u_scale;

uniform vec2 u_size;
varying vec2 v_coords;

uniform vec4 shadow_color;
uniform float sigma;

uniform mat3 input_to_geo;
uniform vec2 geo_size;
uniform vec4 corner_radius;

uniform mat3 window_input_to_geo;
uniform vec2 window_geo_size;
uniform vec4 window_corner_radius;

float gaussian(float x, float s) {
    const float pi = 3.141592653589793;
    return exp(-(x * x) / (2.0 * s * s)) / (sqrt(2.0 * pi) * s);
}

vec2 erf(vec2 x) {
    vec2 s = sign(x), a = abs(x);
    x = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}

float roundedBoxShadowX(float x, float y, float s, float corner, vec2 halfSize) {
    float delta = min(halfSize.y - corner - abs(y), 0.0);
    float curved = halfSize.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    vec2 integral = 0.5 + 0.5 * erf((x + vec2(-curved, curved)) * (sqrt(0.5) / s));
    return integral.y - integral.x;
}

float roundedBoxShadow(vec2 lower, vec2 upper, vec2 point, float s, float corner) {
    vec2 center = (lower + upper) * 0.5;
    vec2 halfSize = (upper - lower) * 0.5;
    point -= center;

    float low = point.y - halfSize.y;
    float high = point.y + halfSize.y;
    float start = clamp(-3.0 * s, low, high);
    float end = clamp(3.0 * s, low, high);

    float step = (end - start) / 4.0;
    float y = start + step * 0.5;
    float value = 0.0;
    for (int i = 0; i < 4; i++) {
        value += roundedBoxShadowX(point.x, point.y - y, s, corner, halfSize) * gaussian(y, s) * step;
        y += step;
    }

    return value;
}

float inner_rounded_rect_alpha(vec2 coords, vec2 size, vec4 corner_radius);

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);
    vec3 coords_window_geo = window_input_to_geo * vec3(v_coords, 1.0);

    vec4 color = shadow_color;

    float shadow_value;
    if (sigma < 0.1) {
        shadow_value = inner_rounded_rect_alpha(coords_geo.xy, geo_size, corner_radius);
    } else {
        shadow_value = roundedBoxShadow(
            vec2(0.0, 0.0),
            geo_size,
            coords_geo.xy,
            sigma,
            corner_radius.x
        );
    }
    color = color * shadow_value;

    if (window_geo_size != vec2(0.0, 0.0)) {
        float win_alpha = inner_rounded_rect_alpha(coords_window_geo.xy, window_geo_size, window_corner_radius);
        color = color * (1.0 - win_alpha);
    }

    color = color * u_alpha;

    gl_FragColor = color;
}