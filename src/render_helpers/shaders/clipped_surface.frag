#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float u_scale;
uniform vec2 geo_size;
uniform vec4 corner_radius;
uniform mat3 input_to_geo;

float rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);

    vec4 color = texture2D(tex, v_coords);

    if (coords_geo.x < 0.0 || 1.0 < coords_geo.x || coords_geo.y < 0.0 || 1.0 < coords_geo.y) {
        color = vec4(0.0);
    } else {
        color = color * rounding_alpha(coords_geo.xy * geo_size, geo_size, corner_radius);
    }

    color = color * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}