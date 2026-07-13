precision highp float;

uniform float u_alpha;
uniform float u_scale;

uniform vec2 u_size;
varying vec2 v_coords;

uniform vec4 color_from;
uniform vec4 color_to;
uniform mat3 input_to_geo;
uniform vec2 geo_size;
uniform vec4 outer_radius;
uniform float border_width;

float rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);
float inner_rounded_rect_alpha(vec2 coords, vec2 size, vec4 corner_radius);

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);
    vec4 color = mix(color_from, color_to, 0.5);
    color = color * rounding_alpha(coords_geo.xy, geo_size, outer_radius);

    if (border_width > 0.0) {
        vec2 inner_coords = coords_geo.xy - vec2(border_width);
        vec2 inner_geo_size = geo_size - vec2(border_width * 2.0);
        vec4 inner_radius = max(outer_radius - vec4(border_width), 0.0);
        color = color * (1.0 - inner_rounded_rect_alpha(inner_coords, inner_geo_size, inner_radius));
    }

    color = color * u_alpha;

    gl_FragColor = color;
}