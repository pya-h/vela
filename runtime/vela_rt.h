#ifndef VELA_RT_H
#define VELA_RT_H

#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <math.h>

static inline void vela_show_int(int64_t x) {
    printf("%ld\n", (long)x);
}

static inline void vela_show_float(double x) {
    printf("%g\n", x);
}

static inline void vela_show_bool(bool x) {
    printf("%s\n", x ? "true" : "false");
}

static inline void vela_show_text(const char* x) {
    printf("%s\n", x);
}

/* Float remainder (Vela's `%` on floats); the result has the dividend's sign. */
static inline double vela_fmod(double a, double b) {
    return fmod(a, b);
}

static inline void* vela_new(size_t size) {
    return malloc(size);
}

static inline void vela_free(void* p) {
    free(p);
}

#endif
