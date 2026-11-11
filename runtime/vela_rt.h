#ifndef VELA_RT_H
#define VELA_RT_H

/* Generated Vela names never contain __, preventing collisions with vela__rt__*. */

#ifndef _POSIX_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#endif
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE
#endif

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include <sys/types.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <fcntl.h>
#include <poll.h>

static inline void vela__rt__show_line(void) {
    putchar('\n');
}

static inline void vela__rt__write_int(int64_t x) {
    /* `long long` is at least 64 bits everywhere; `long` is 32 bits on Windows. */
    printf("%lld", (long long)x);
}

static inline void vela__rt__show_int(int64_t x) {
    vela__rt__write_int(x);
    vela__rt__show_line();
}

static inline void vela__rt__write_uint(uint64_t x) {
    printf("%llu", (unsigned long long)x);
}

static inline void vela__rt__show_uint(uint64_t x) {
    vela__rt__write_uint(x);
    vela__rt__show_line();
}

static inline void vela__rt__write_float(double x) {
    /* C libraries print a NaN's sign inconsistently (`-nan` or `nan`). */
    if (isnan(x)) {
        fputs("nan", stdout);
        return;
    }
    printf("%g", x);
}

static inline void vela__rt__show_float(double x) {
    vela__rt__write_float(x);
    vela__rt__show_line();
}

static inline void vela__rt__write_bool(bool x) {
    fputs(x ? "true" : "false", stdout);
}

static inline void vela__rt__show_bool(bool x) {
    vela__rt__write_bool(x);
    vela__rt__show_line();
}

/* Bytes without a trailing '\0'; interior '\0' bytes are preserved in len. */
typedef struct vela__rt__text {
    const char* bytes;
    int64_t len;
} vela__rt__text;

/* Suppresses unused-function warnings on generated static inline helpers. */
#if defined(__GNUC__) || defined(__clang__)
#define VELA__RT__MAYBE_UNUSED __attribute__((unused))
#else
#define VELA__RT__MAYBE_UNUSED
#endif

#define VELA__RT__TEXT(literal) ((vela__rt__text){ (literal), (int64_t)sizeof(literal) - 1 })

static inline void vela__rt__write_text(vela__rt__text x) {
    if (x.len > 0) {
        fwrite(x.bytes, 1, (size_t)x.len, stdout);
    }
}

static inline void vela__rt__show_text(vela__rt__text x) {
    vela__rt__write_text(x);
    vela__rt__show_line();
}

static inline void vela__rt__write_char(uint8_t c) {
    putchar(c);
}

static inline void vela__rt__show_char(uint8_t c) {
    vela__rt__write_char(c);
    vela__rt__show_line();
}

static inline bool vela__rt__text_eq(vela__rt__text a, vela__rt__text b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.bytes, b.bytes, (size_t)a.len) == 0);
}

/* Unsigned byte order matches Unicode code point order for UTF-8. */
static inline int vela__rt__text_order(vela__rt__text a, vela__rt__text b) {
    int64_t shorter = a.len < b.len ? a.len : b.len;
    int order = shorter == 0 ? 0 : memcmp(a.bytes, b.bytes, (size_t)shorter);
    if (order != 0) {
        return order;
    }
    return (a.len > b.len) - (a.len < b.len);
}

static inline void vela__rt__decorator_failed(const char* decorator, const char* task, vela__rt__text error, bool stops) {
    fflush(stdout);
    fprintf(stderr, "vela: decorator '%s' failed on task '%s'%s: ", decorator, task, stops ? "" : ", and the program goes on");
    /* fwrite preserves interior '\0' bytes. */
    fwrite(error.bytes, 1, (size_t)error.len, stderr);
    fputc('\n', stderr);
    if (stops) {
        abort();
    }
}

static inline _Noreturn void vela__rt__int_trap(const char* what, const char* at) {
    fflush(stdout);
    fprintf(stderr, "vela: %s at %s\n", what, at);
    abort();
}

#if defined(__GNUC__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
#define VELA__RT__OVERFLOW_BUILTINS 1
#else
#define VELA__RT__OVERFLOW_BUILTINS 0
#endif

static inline bool vela__rt__add_overflows(int64_t a, int64_t b, int64_t* r) {
#if VELA__RT__OVERFLOW_BUILTINS
    return __builtin_add_overflow(a, b, r);
#else
    if ((b > 0 && a > INT64_MAX - b) || (b < 0 && a < INT64_MIN - b)) {
        return true;
    }
    *r = a + b;
    return false;
#endif
}

static inline bool vela__rt__sub_overflows(int64_t a, int64_t b, int64_t* r) {
#if VELA__RT__OVERFLOW_BUILTINS
    return __builtin_sub_overflow(a, b, r);
#else
    if ((b < 0 && a > INT64_MAX + b) || (b > 0 && a < INT64_MIN + b)) {
        return true;
    }
    *r = a - b;
    return false;
#endif
}

static inline bool vela__rt__mul_overflows(int64_t a, int64_t b, int64_t* r) {
#if VELA__RT__OVERFLOW_BUILTINS
    return __builtin_mul_overflow(a, b, r);
#else
    bool overflows;
    if (a > 0) {
        overflows = b > 0 ? a > INT64_MAX / b : b < INT64_MIN / a;
    } else if (a < 0) {
        overflows = b > 0 ? a < INT64_MIN / b : b < 0 && a < INT64_MAX / b;
    } else {
        overflows = false;
    }
    if (overflows) {
        return true;
    }
    *r = a * b;
    return false;
#endif
}

static inline int64_t vela__rt__add(int64_t a, int64_t b, const char* at) {
    int64_t r;
    if (vela__rt__add_overflows(a, b, &r)) {
        vela__rt__int_trap("integer overflow", at);
    }
    return r;
}

static inline int64_t vela__rt__sub(int64_t a, int64_t b, const char* at) {
    int64_t r;
    if (vela__rt__sub_overflows(a, b, &r)) {
        vela__rt__int_trap("integer overflow", at);
    }
    return r;
}

static inline int64_t vela__rt__mul(int64_t a, int64_t b, const char* at) {
    int64_t r;
    if (vela__rt__mul_overflows(a, b, &r)) {
        vela__rt__int_trap("integer overflow", at);
    }
    return r;
}

/* INT64_MIN / -1 is the only signed division quotient that overflows int64_t. */
static inline int64_t vela__rt__div(int64_t a, int64_t b, const char* at) {
    if (b == 0) {
        vela__rt__int_trap("division by zero", at);
    }
    if (b == -1 && a == INT64_MIN) {
        vela__rt__int_trap("integer overflow", at);
    }
    return a / b;
}

/* INT64_MIN % -1 is UB in C even though the true mathematical remainder is 0. */
static inline int64_t vela__rt__mod(int64_t a, int64_t b, const char* at) {
    if (b == 0) {
        vela__rt__int_trap("remainder of division by zero", at);
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

static inline int64_t vela__rt__neg(int64_t a, const char* at) {
    if (a == INT64_MIN) {
        vela__rt__int_trap("integer overflow", at);
    }
    return -a;
}

/* Special-case bases 2, 10, and e for exact results without log(x)/log(base) drift. */
static inline double vela__rt__math_log(double x, double base) {
    if (base == 2.0) {
        return log2(x);
    }
    if (base == 10.0) {
        return log10(x);
    }
    if (base == 2.718281828459045) {
        return log(x);
    }
    return log(x) / log(base);
}

/* Any squaring overflow means the final power would overflow too. */
static inline int64_t vela__rt__pow(int64_t a, int64_t b, const char* at) {
    int64_t result = 1;
    while (b > 0) {
        if (b & 1) {
            result = vela__rt__mul(result, a, at);
        }
        b >>= 1;
        if (b > 0) {
            a = vela__rt__mul(a, a, at);
        }
    }
    return result;
}

static inline uint8_t vela__rt__unit_pow(uint8_t a, uint8_t b, const char* at) {
    int64_t result = 1;
    for (unsigned i = 0; i < b; i++) {
        result *= a;
        if (result > 255) {
            char written[64];
            snprintf(written, sizeof written, "%u ** %u does not fit in a unit", (unsigned)a, (unsigned)b);
            vela__rt__int_trap(written, at);
        }
    }
    return (uint8_t)result;
}

/* Computed in int64_t so intermediate unit arithmetic cannot wrap silently. */
static inline uint8_t vela__rt__unit_of(int64_t x, const char* at) {
    if (x < 0 || x > 255) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in a unit", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (uint8_t)x;
}

static inline uint8_t vela__rt__char_of(int64_t x, const char* at) {
    if (x < 0 || x > 255) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in a char", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (uint8_t)x;
}

static inline int8_t vela__rt__int8_of(int64_t x, const char* at) {
    if (x < -128 || x > 127) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in an int8", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (int8_t)x;
}

static inline int16_t vela__rt__int16_of(int64_t x, const char* at) {
    if (x < -32768 || x > 32767) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in an int16", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (int16_t)x;
}

static inline int32_t vela__rt__int32_of(int64_t x, const char* at) {
    if (x < -2147483648LL || x > 2147483647LL) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in an int32", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (int32_t)x;
}

static inline uint16_t vela__rt__uint16_of(int64_t x, const char* at) {
    if (x < 0 || x > 65535) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in a uint16", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (uint16_t)x;
}

static inline uint32_t vela__rt__uint32_of(int64_t x, const char* at) {
    if (x < 0 || x > 4294967295LL) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in a uint32", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (uint32_t)x;
}

static inline uint64_t vela__rt__uint64_of(int64_t x, const char* at) {
    if (x < 0) {
        char written[64];
        snprintf(written, sizeof written, "%lld does not fit in a uint64", (long long)x);
        vela__rt__int_trap(written, at);
    }
    return (uint64_t)x;
}

static inline int64_t vela__rt__int_of_uint(uint64_t x, const char* at) {
    if (x > (uint64_t)INT64_MAX) {
        char written[64];
        snprintf(written, sizeof written, "%llu does not fit in an int", (unsigned long long)x);
        vela__rt__int_trap(written, at);
    }
    return (int64_t)x;
}

static inline uint64_t vela__rt__uint_add(uint64_t a, uint64_t b, const char* at) {
    if (a > UINT64_MAX - b) {
        vela__rt__int_trap("integer overflow", at);
    }
    return a + b;
}

static inline uint64_t vela__rt__uint_sub(uint64_t a, uint64_t b, const char* at) {
    if (b > a) {
        vela__rt__int_trap("integer overflow", at);
    }
    return a - b;
}

static inline uint64_t vela__rt__uint_mul(uint64_t a, uint64_t b, const char* at) {
    if (a != 0 && b > UINT64_MAX / a) {
        vela__rt__int_trap("integer overflow", at);
    }
    return a * b;
}

static inline uint64_t vela__rt__uint_div(uint64_t a, uint64_t b, const char* at) {
    if (b == 0) {
        vela__rt__int_trap("division by zero", at);
    }
    return a / b;
}

static inline uint64_t vela__rt__uint_mod(uint64_t a, uint64_t b, const char* at) {
    if (b == 0) {
        vela__rt__int_trap("remainder of division by zero", at);
    }
    return a % b;
}

static inline uint64_t vela__rt__uint_pow(uint64_t a, uint64_t b, const char* at) {
    uint64_t result = 1;
    while (b > 0) {
        if (b & 1) {
            result = vela__rt__uint_mul(result, a, at);
        }
        b >>= 1;
        if (b > 0) {
            a = vela__rt__uint_mul(a, a, at);
        }
    }
    return result;
}

static inline double vela__rt__fmod(double a, double b) {
    return fmod(a, b);
}

/* (double)INT64_MIN == -2^63 is exact in IEEE 754; +2^63 is the first out-of-range double, and NaN fails both bounds. */
static inline int64_t vela__rt__int_of_float(double x, const char* at) {
    if (isnan(x)) {
        vela__rt__int_trap("nan cannot become an int", at);
    }
    if (!(x >= (double)INT64_MIN && x < -(double)INT64_MIN)) {
        char written[64];
        snprintf(written, sizeof written, "%g does not fit in an int", x);
        vela__rt__int_trap(written, at);
    }
    return (int64_t)x;
}

enum vela__rt__read {
    VELA__RT__READ_OK = 0,
    VELA__RT__READ_NOT_A_NUMBER = 1,
    VELA__RT__READ_TOO_LARGE = 2,
    VELA__RT__READ_ENDED = 3,
};

static inline int vela__rt__int_of_text(vela__rt__text t, int64_t* value) {
    size_t len = t.len < 0 ? 0 : (size_t)t.len;
    size_t i = 0;
    bool negative = len > 0 && t.bytes[0] == '-';
    if (negative) {
        i = 1;
    }
    if (i >= len) {
        return VELA__RT__READ_NOT_A_NUMBER;
    }
    /* Accumulate magnitude unsigned so INT64_MIN parses without signed overflow. */
    const uint64_t smallest = (uint64_t)INT64_MAX + 1u;
    uint64_t limit = negative ? smallest : (uint64_t)INT64_MAX;
    uint64_t base = 10u;
    if (len - i > 2 && t.bytes[i] == '0') {
        switch (t.bytes[i + 1]) {
        case 'x': base = 16u; break;
        case 'o': base = 8u; break;
        case 'b': base = 2u; break;
        default: break;
        }
        if (base != 10u) {
            i += 2;
        }
    }
    uint64_t digits = 0;
    for (; i < len; i++) {
        unsigned char c = (unsigned char)t.bytes[i];
        uint64_t digit = base;
        if (c >= '0' && c <= '9') {
            digit = (uint64_t)(c - '0');
        } else if (c >= 'a' && c <= 'f') {
            digit = (uint64_t)(c - 'a') + 10u;
        } else if (c >= 'A' && c <= 'F') {
            digit = (uint64_t)(c - 'A') + 10u;
        }
        if (digit >= base) {
            return VELA__RT__READ_NOT_A_NUMBER;
        }
        if (digits > (limit - digit) / base) {
            return VELA__RT__READ_TOO_LARGE;
        }
        digits = digits * base + digit;
    }
    *value = digits == smallest ? INT64_MIN : (negative ? -(int64_t)digits : (int64_t)digits);
    return VELA__RT__READ_OK;
}

/* Bounds stack-buffered digits far beyond double's ~17 significant digits. */
#define VELA__RT__READ_DIGITS 400

static inline int vela__rt__float_of_text(vela__rt__text t, double* value) {
    size_t len = t.len < 0 ? 0 : (size_t)t.len;
    size_t i = 0;
    bool negative = len > 0 && t.bytes[0] == '-';
    if (negative) {
        i = 1;
    }
    char digits[VELA__RT__READ_DIGITS];
    size_t kept = 0;
    size_t dropped = 0;
    size_t whole = 0;
    size_t fraction = 0;
    bool dot = false;
    for (; i < len; i++) {
        unsigned char c = (unsigned char)t.bytes[i];
        if (c == '.') {
            if (dot) {
                return VELA__RT__READ_NOT_A_NUMBER;
            }
            dot = true;
            continue;
        }
        if (c < '0' || c > '9') {
            return VELA__RT__READ_NOT_A_NUMBER;
        }
        if (dot) {
            fraction++;
        } else {
            whole++;
        }
        if (kept == 0 && c == '0') {
            continue;
        }
        if (kept < VELA__RT__READ_DIGITS) {
            digits[kept++] = (char)c;
        } else {
            dropped++;
        }
    }
    if (whole == 0 || (dot && fraction == 0)) {
        return VELA__RT__READ_NOT_A_NUMBER;
    }
    if (kept == 0) {
        *value = negative ? -0.0 : 0.0;
        return VELA__RT__READ_OK;
    }
    /* Balance truncated significant digits against fractional digit count. */
    long exponent = (long)dropped - (long)fraction;
    char written[VELA__RT__READ_DIGITS + 32];
    int size = snprintf(written, sizeof written, "%s%.*se%ld", negative ? "-" : "", (int)kept, digits, exponent);
    if (size < 0 || (size_t)size >= sizeof written) {
        return VELA__RT__READ_NOT_A_NUMBER;
    }
    char* after = NULL;
    double read = strtod(written, &after);
    if (after != written + size) {
        return VELA__RT__READ_NOT_A_NUMBER;
    }
    if (isinf(read)) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = read;
    return VELA__RT__READ_OK;
}

typedef struct vela__rt__chunk {
    struct vela__rt__chunk* previous;
    size_t size;
    size_t used;
    /* Aligned for any type, so the first allocation needs no padding. */
    _Alignas(max_align_t) unsigned char data[];
} vela__rt__chunk;

/* Region-allocated cleanup hooks, invoked in reverse order before chunks are freed. */
typedef struct vela__rt__closer {
    struct vela__rt__closer* next;
    int64_t handle;
    void (*close_fn)(int64_t handle);
} vela__rt__closer;

typedef struct vela__rt__region {
    vela__rt__chunk* newest;
    vela__rt__closer* closers;
} vela__rt__region;

#define VELA__RT__REGION_EMPTY { NULL, NULL }

enum { VELA__RT__CHUNK_FIRST = 4096, VELA__RT__CHUNK_MAX = 1 << 20 };

static inline _Noreturn void vela__rt__out_of_memory(void) {
    fflush(stdout);
    fputs("vela: out of memory\n", stderr);
    abort();
}

/* Double ordinary chunks up to 1 MB; oversized allocs go behind newest so small bumps keep the tip. */
static inline void* vela__rt__region_grow(vela__rt__region* r, size_t size) {
    size_t header = sizeof(vela__rt__chunk);
    size_t total = VELA__RT__CHUNK_FIRST;
    if (r->newest != NULL) {
        size_t previous = r->newest->size + header;
        bool ordinary = previous <= VELA__RT__CHUNK_MAX && (previous & (previous - 1)) == 0;
        if (ordinary) {
            total = previous >= VELA__RT__CHUNK_MAX / 2 ? VELA__RT__CHUNK_MAX : previous * 2;
        }
    }
    size_t capacity = total - header;
    bool own = r->newest != NULL ? size > capacity / 2 : size > capacity;
    if (own) {
        capacity = size;
    }
    if (capacity > SIZE_MAX - header) {
        vela__rt__out_of_memory();
    }
    vela__rt__chunk* chunk = malloc(header + capacity);
    if (chunk == NULL) {
        vela__rt__out_of_memory();
    }
    chunk->size = capacity;
    chunk->used = size;
    if (own && r->newest != NULL) {
        chunk->previous = r->newest->previous;
        r->newest->previous = chunk;
    } else {
        chunk->previous = r->newest;
        r->newest = chunk;
    }
    return chunk->data;
}

/* Ensures allocation sits at newest->data's tip so repeated joins grow in place. */
static inline void* vela__rt__region_grow_newest(vela__rt__region* r, size_t size, size_t capacity) {
    size_t header = sizeof(vela__rt__chunk);
    if (capacity < size) {
        capacity = size;
    }
    if (capacity > SIZE_MAX - header) {
        vela__rt__out_of_memory();
    }
    vela__rt__chunk* chunk = malloc(header + capacity);
    if (chunk == NULL) {
        vela__rt__out_of_memory();
    }
    chunk->size = capacity;
    chunk->used = size;
    chunk->previous = r->newest;
    r->newest = chunk;
    return chunk->data;
}

static inline void* vela__rt__region_alloc(vela__rt__region* r, size_t size, size_t align) {
    vela__rt__chunk* chunk = r->newest;
    if (chunk != NULL) {
        size_t start = (chunk->used + (align - 1)) & ~(align - 1);
        if (start <= chunk->size && size <= chunk->size - start) {
            chunk->used = start + size;
            return chunk->data + start;
        }
    }
    return vela__rt__region_grow(r, size);
}

static inline void* vela__rt__new(vela__rt__region* r, const void* value, size_t size, size_t align) {
    void* p = vela__rt__region_alloc(r, size, align);
    memcpy(p, value, size);
    return p;
}

static inline int64_t vela__rt__index(int64_t i, int64_t len, const char* at) {
    if (i < 0 || i >= len) {
        fflush(stdout);
        fprintf(stderr, "vela: index %lld is out of range 0..%lld at %s\n", (long long)i, (long long)len, at);
        abort();
    }
    return i;
}

static inline void vela__rt__part(int64_t from, int64_t to, int64_t len, const char* what, const char* at) {
    if (to < from) {
        fflush(stdout);
        fprintf(stderr, "vela: %s %lld..%lld ends before it starts at %s\n", what, (long long)from, (long long)to, at);
        abort();
    }
    if (from < 0 || to > len) {
        fflush(stdout);
        fprintf(stderr, "vela: %s %lld..%lld is out of range 0..%lld at %s\n", what, (long long)from, (long long)to, (long long)len, at);
        abort();
    }
}

/* Rejects slice bounds landing mid-UTF-8 continuation byte (0b10xxxxxx). */
static inline vela__rt__text vela__rt__text_part(vela__rt__text text, int64_t from, int64_t to, const char* at) {
    vela__rt__part(from, to, text.len, "text", at);
    int64_t bounds[2] = { from, to };
    for (int i = 0; i < 2; i++) {
        int64_t bound = bounds[i];
        if (bound < text.len && ((unsigned char)text.bytes[bound] & 0xC0) == 0x80) {
            fflush(stdout);
            fprintf(stderr, "vela: text %lld..%lld cuts through a character at byte %lld at %s\n", (long long)from, (long long)to,
                    (long long)bound, at);
            abort();
        }
    }
    return (vela__rt__text){ text.bytes + from, to - from };
}

/* Per-run hash seed thwarts hash flooding without changing insertion-ordered iteration. */
static inline uint64_t vela__rt__hash_seed(void) {
    static uint64_t seed = 0;
    if (seed == 0) {
        uint64_t local = 0;
        seed = ((uint64_t)time(NULL) * UINT64_C(0x9e3779b97f4a7c15)) ^ (uint64_t)(uintptr_t)&local ^ UINT64_C(0x243f6a8885a308d3);
        seed |= 1;
    }
    return seed;
}

static inline uint64_t vela__rt__mix(uint64_t x) {
    x ^= x >> 32;
    x *= UINT64_C(0xd6e8feb86659fd93);
    x ^= x >> 32;
    x *= UINT64_C(0xd6e8feb86659fd93);
    x ^= x >> 32;
    return x;
}

static inline uint64_t vela__rt__hash_bytes(uint64_t h, const char* bytes, int64_t len) {
    size_t n = len < 0 ? 0 : (size_t)len;
    size_t i = 0;
    for (; i + 8 <= n; i += 8) {
        uint64_t word;
        memcpy(&word, bytes + i, 8);
        h = (h ^ word) * UINT64_C(0x100000001b3);
        h ^= h >> 29;
    }
    uint64_t tail = 0;
    if (i < n) {
        memcpy(&tail, bytes + i, n - i);
    }
    return vela__rt__mix(h ^ tail ^ ((uint64_t)n << 56));
}

/* A hash as a map keeps it: never 0, which marks an entry taken out. */
static inline uint64_t vela__rt__kept_hash(uint64_t h) {
    return h == 0 ? 1 : h;
}

static inline _Noreturn void vela__rt__no_key(const char* at) {
    vela__rt__int_trap("no such key in the map", at);
}

/* Slot indices are signed 32-bit ints, capping map capacity below 2^30. */
static inline void* vela__rt__map_room(vela__rt__region* r, int64_t count, size_t size, size_t align, bool zero) {
    if (count > (INT64_C(1) << 31)) {
        fflush(stdout);
        fputs("vela: a map can't hold more than 2^30 keys\n", stderr);
        abort();
    }
    void* room = vela__rt__region_alloc(r, (size_t)count * size, align);
    if (zero) {
        memset(room, 0, (size_t)count * size);
    }
    return room;
}

static inline uint8_t vela__rt__text_at(vela__rt__text t, int64_t i, const char* at) {
    return (uint8_t)t.bytes[vela__rt__index(i, t.len, at)];
}

static inline void* vela__rt__region_items(vela__rt__region* r, int64_t len, size_t size, size_t align, const char* at) {
    if (len < 0) {
        fflush(stdout);
        fprintf(stderr, "vela: count %lld is negative at %s\n", (long long)len, at);
        abort();
    }
    if ((uint64_t)len > SIZE_MAX / size) {
        vela__rt__out_of_memory();
    }
    return vela__rt__region_alloc(r, (size_t)len * size, align);
}

/* Empty slices still require a non-NULL aligned pointer. */
static inline void* vela__rt__no_items(void) {
    static _Alignas(max_align_t) char none[1];
    return none;
}

/* Copy-on-alias slice buffer: appends grow in place only when their end matches room->end. */

typedef struct vela__rt__room {
    char* end;
    char* limit;
} vela__rt__room;

static inline _Noreturn void vela__rt__empty_trap(const char* at) {
    fflush(stdout);
    fprintf(stderr, "vela: del() on an empty slice at %s\n", at);
    abort();
}

static inline void vela__rt__not_empty_for(int64_t len, const char* what, const char* at) {
    if (len == 0) {
        fflush(stdout);
        fprintf(stderr, "vela: %s on an empty slice at %s\n", what, at);
        abort();
    }
}

static inline void vela__rt__not_empty(int64_t len, const char* at) {
    if (len == 0) {
        vela__rt__empty_trap(at);
    }
}

static inline void vela__rt__room_extend(vela__rt__region* r, vela__rt__room* room, size_t wanted, size_t size) {
    vela__rt__chunk* chunk = r->newest;
    if (chunk == NULL || room->limit != (char*)chunk->data + chunk->used) {
        return;
    }
    size_t spare = chunk->size - chunk->used;
    size_t more = wanted < size ? size : wanted;
    if (more > spare) {
        more = spare - spare % size;
    }
    chunk->used += more;
    room->limit += more;
}

static inline void* vela__rt__items_insert(vela__rt__region* r, void* items, int64_t len, vela__rt__room** room, int64_t index,
                                           size_t size, size_t align) {
    char* old = items;
    size_t n = (size_t)len;
    size_t i = (size_t)index;
    vela__rt__room* own = *room;
    if (own != NULL && old + n * size == own->end) {
        if ((size_t)(own->limit - own->end) < size) {
            vela__rt__room_extend(r, own, n * size, size);
        }
        if ((size_t)(own->limit - own->end) >= size) {
            memmove(old + (i + 1) * size, old + i * size, (n - i) * size);
            own->end += size;
            return old;
        }
    }
    size_t header = (sizeof(vela__rt__room) + align - 1) & ~(align - 1);
    if (n > (SIZE_MAX - header) / size / 2 - 4) {
        vela__rt__out_of_memory();
    }
    size_t capacity = n < 4 ? 4 : 2 * n;
    size_t block_align = align > _Alignof(vela__rt__room) ? align : _Alignof(vela__rt__room);
    char* block = vela__rt__region_alloc(r, header + capacity * size, block_align);
    char* fresh = block + header;
    memcpy(fresh, old, i * size);
    memcpy(fresh + (i + 1) * size, old + i * size, (n - i) * size);
    vela__rt__room* made = (vela__rt__room*)(void*)block;
    made->end = fresh + (n + 1) * size;
    made->limit = fresh + capacity * size;
    *room = made;
    return fresh;
}

static inline void vela__rt__items_remove(void* items, int64_t len, vela__rt__room* room, int64_t index, size_t size) {
    char* at = items;
    size_t n = (size_t)len;
    size_t i = (size_t)index;
    memmove(at + i * size, at + (i + 1) * size, (n - i - 1) * size);
    if (room != NULL && at + n * size == room->end) {
        room->end -= size;
    }
}

static inline vela__rt__text vela__rt__join(vela__rt__region* r, const vela__rt__text* parts, size_t count) {
    uint64_t len = 0;
    for (size_t i = 0; i < count; i++) {
        if ((uint64_t)parts[i].len > (uint64_t)INT64_MAX - len) {
            vela__rt__out_of_memory();
        }
        len += (uint64_t)parts[i].len;
    }
    if (len > SIZE_MAX) {
        vela__rt__out_of_memory();
    }
    /* Fast path: text already at the newest chunk tip grows in place across += chains. */
    vela__rt__chunk* chunk = r->newest;
    bool at_tip = count > 0 && parts[0].len > 0 && chunk != NULL && parts[0].bytes + parts[0].len == (const char*)chunk->data + chunk->used;
    if (at_tip) {
        size_t rest = (size_t)len - (size_t)parts[0].len;
        if (rest <= chunk->size - chunk->used) {
            char* tail = (char*)chunk->data + chunk->used;
            chunk->used += rest;
            for (size_t i = 1; i < count; i++) {
                if (parts[i].len > 0) {
                    memcpy(tail, parts[i].bytes, (size_t)parts[i].len);
                    tail += parts[i].len;
                }
            }
            return (vela__rt__text){ parts[0].bytes, (int64_t)len };
        }
    }
    char* bytes = !at_tip ? vela__rt__region_alloc(r, (size_t)len, 1)
                          : vela__rt__region_grow_newest(r, (size_t)len, (size_t)len <= SIZE_MAX / 2 ? (size_t)len * 2 : (size_t)len);
    size_t at = 0;
    for (size_t i = 0; i < count; i++) {
        if (parts[i].len > 0) {
            memcpy(bytes + at, parts[i].bytes, (size_t)parts[i].len);
            at += (size_t)parts[i].len;
        }
    }
    return (vela__rt__text){ bytes, (int64_t)len };
}

static inline vela__rt__text vela__rt__text_in(vela__rt__region* r, const char* bytes, size_t len) {
    char* copy = vela__rt__region_alloc(r, len, 1);
    if (len > 0) {
        memcpy(copy, bytes, len);
    }
    return (vela__rt__text){ copy, (int64_t)len };
}

static inline vela__rt__text vela__rt__int_str(vela__rt__region* r, int64_t x) {
    char digits[24];
    int len = snprintf(digits, sizeof digits, "%lld", (long long)x);
    return vela__rt__text_in(r, digits, (size_t)len);
}

static inline vela__rt__text vela__rt__uint_str(vela__rt__region* r, uint64_t x) {
    char digits[24];
    int len = snprintf(digits, sizeof digits, "%llu", (unsigned long long)x);
    return vela__rt__text_in(r, digits, (size_t)len);
}

static inline vela__rt__text vela__rt__char_str(vela__rt__region* r, uint8_t c) {
    char byte = (char)c;
    return vela__rt__text_in(r, &byte, 1);
}

static inline vela__rt__text vela__rt__float_str(vela__rt__region* r, double x) {
    if (isnan(x)) {
        return vela__rt__text_in(r, "nan", 3);
    }
    char digits[32];
    int len = snprintf(digits, sizeof digits, "%g", x);
    return vela__rt__text_in(r, digits, (size_t)len);
}

static inline vela__rt__text vela__rt__bool_str(vela__rt__region* r, bool x) {
    (void)r;
    return x ? VELA__RT__TEXT("true") : VELA__RT__TEXT("false");
}

static inline vela__rt__text vela__rt__text_str(vela__rt__region* r, vela__rt__text x) {
    return vela__rt__text_in(r, x.bytes, (size_t)x.len);
}

/* Stack scratch buffer for formatting numbers inside joins without region allocation. */
#define VELA__RT__NUMBER_ROOM 32

static inline vela__rt__text vela__rt__int_view(char* room, int64_t x) {
    int len = snprintf(room, VELA__RT__NUMBER_ROOM, "%lld", (long long)x);
    return (vela__rt__text){ room, (int64_t)len };
}

static inline vela__rt__text vela__rt__uint_view(char* room, uint64_t x) {
    int len = snprintf(room, VELA__RT__NUMBER_ROOM, "%llu", (unsigned long long)x);
    return (vela__rt__text){ room, (int64_t)len };
}

static inline vela__rt__text vela__rt__char_view(char* room, uint8_t c) {
    room[0] = (char)c;
    return (vela__rt__text){ room, 1 };
}

static inline vela__rt__text vela__rt__float_view(char* room, double x) {
    if (isnan(x)) {
        return VELA__RT__TEXT("nan");
    }
    int len = snprintf(room, VELA__RT__NUMBER_ROOM, "%g", x);
    return (vela__rt__text){ room, (int64_t)len };
}

typedef struct vela__rt__writer {
    bool to_output;
    char* bytes;
    size_t len;
    size_t cap;
} vela__rt__writer;

#define VELA__RT__OUTPUT ((vela__rt__writer){ true, NULL, 0, 0 })
#define VELA__RT__BUILDER ((vela__rt__writer){ false, NULL, 0, 0 })

static inline void vela__rt__put(vela__rt__writer* w, const char* bytes, size_t n) {
    if (n == 0) {
        return;
    }
    if (w->to_output) {
        fwrite(bytes, 1, n, stdout);
        return;
    }
    if (w->cap - w->len < n) {
        if (n > SIZE_MAX / 2 - w->len) {
            vela__rt__out_of_memory();
        }
        size_t cap = w->cap < 64 ? 64 : w->cap;
        while (cap - w->len < n) {
            cap *= 2;
        }
        char* grown = realloc(w->bytes, cap);
        if (grown == NULL) {
            vela__rt__out_of_memory();
        }
        w->bytes = grown;
        w->cap = cap;
    }
    memcpy(w->bytes + w->len, bytes, n);
    w->len += n;
}

#define VELA__RT__PUT(w, literal) vela__rt__put((w), (literal), sizeof(literal) - 1)

static inline void vela__rt__put_int(vela__rt__writer* w, int64_t x) {
    char room[VELA__RT__NUMBER_ROOM];
    vela__rt__text t = vela__rt__int_view(room, x);
    vela__rt__put(w, t.bytes, (size_t)t.len);
}

static inline void vela__rt__put_uint(vela__rt__writer* w, uint64_t x) {
    char room[VELA__RT__NUMBER_ROOM];
    vela__rt__text t = vela__rt__uint_view(room, x);
    vela__rt__put(w, t.bytes, (size_t)t.len);
}

static inline void vela__rt__put_float(vela__rt__writer* w, double x) {
    char room[VELA__RT__NUMBER_ROOM];
    vela__rt__text t = vela__rt__float_view(room, x);
    vela__rt__put(w, t.bytes, (size_t)t.len);
}

static inline void vela__rt__put_bool(vela__rt__writer* w, bool x) {
    if (x) {
        VELA__RT__PUT(w, "true");
    } else {
        VELA__RT__PUT(w, "false");
    }
}

static inline void vela__rt__put_text(vela__rt__writer* w, vela__rt__text x) {
    vela__rt__put(w, x.bytes, x.len < 0 ? 0 : (size_t)x.len);
}

/* Escapes '{' alongside quotes/backslashes so printed text remains a valid Vela literal. */
static inline void vela__rt__put_quoted(vela__rt__writer* w, vela__rt__text x) {
    VELA__RT__PUT(w, "\"");
    size_t from = 0;
    size_t len = x.len < 0 ? 0 : (size_t)x.len;
    for (size_t i = 0; i < len; i++) {
        const char* escape = NULL;
        switch (x.bytes[i]) {
        case '"': escape = "\\\""; break;
        case '\\': escape = "\\\\"; break;
        case '\n': escape = "\\n"; break;
        case '\t': escape = "\\t"; break;
        case '\r': escape = "\\r"; break;
        case '\0': escape = "\\0"; break;
        case '{': escape = "\\{"; break;
        case '}': escape = "\\}"; break;
        default: break;
        }
        if (escape != NULL) {
            vela__rt__put(w, x.bytes + from, i - from);
            vela__rt__put(w, escape, 2);
            from = i + 1;
        }
    }
    vela__rt__put(w, x.bytes + from, len - from);
    VELA__RT__PUT(w, "\"");
}

/* Writes control and non-ASCII bytes as char(n) to keep printed representations ASCII. */
static inline void vela__rt__put_char(vela__rt__writer* w, uint8_t c) {
    const char* escape = NULL;
    switch (c) {
    case '\'': escape = "'\\''"; break;
    case '\\': escape = "'\\\\'"; break;
    case '\n': escape = "'\\n'"; break;
    case '\t': escape = "'\\t'"; break;
    case '\r': escape = "'\\r'"; break;
    case '\0': escape = "'\\0'"; break;
    default: break;
    }
    if (escape != NULL) {
        vela__rt__put(w, escape, 4);
        return;
    }
    if (c < 32 || c >= 127) {
        char room[16];
        int len = snprintf(room, sizeof room, "char(%u)", (unsigned)c);
        vela__rt__put(w, room, (size_t)len);
        return;
    }
    char quoted[3] = { '\'', (char)c, '\'' };
    vela__rt__put(w, quoted, 3);
}

static inline void vela__rt__put_address(vela__rt__writer* w, const void* p) {
    char room[VELA__RT__NUMBER_ROOM];
    int len = snprintf(room, sizeof room, "0x%llx", (unsigned long long)(uintptr_t)p);
    vela__rt__put(w, room, (size_t)len);
}

static inline vela__rt__text vela__rt__built(vela__rt__writer* w, vela__rt__region* r) {
    vela__rt__text t = vela__rt__text_in(r, w->bytes == NULL ? "" : w->bytes, w->len);
    free(w->bytes);
    w->bytes = NULL;
    w->len = w->cap = 0;
    return t;
}

/* Escapes a string according to RFC 8259 JSON format. */
static inline void vela__rt__put_json_text(vela__rt__writer* w, vela__rt__text x) {
    VELA__RT__PUT(w, "\"");
    size_t from = 0;
    size_t len = x.len < 0 ? 0 : (size_t)x.len;
    for (size_t i = 0; i < len; i++) {
        uint8_t c = (uint8_t)x.bytes[i];
        const char* escape = NULL;
        switch (c) {
        case '"': escape = "\\\""; break;
        case '\\': escape = "\\\\"; break;
        case '\n': escape = "\\n"; break;
        case '\t': escape = "\\t"; break;
        case '\r': escape = "\\r"; break;
        case '\b': escape = "\\b"; break;
        case '\f': escape = "\\f"; break;
        default: break;
        }
        if (escape != NULL) {
            vela__rt__put(w, x.bytes + from, i - from);
            vela__rt__put(w, escape, 2);
            from = i + 1;
        } else if (c < 0x20) {
            /* The other control characters have no short escape. */
            char code[7];
            snprintf(code, sizeof code, "\\u%04x", (unsigned)c);
            vela__rt__put(w, x.bytes + from, i - from);
            vela__rt__put(w, code, 6);
            from = i + 1;
        }
    }
    vela__rt__put(w, x.bytes + from, len - from);
    VELA__RT__PUT(w, "\"");
}

/* A float as JSON: the fewest digits that read back as the same number (as a 32-bit float
   when `single`), and null for nan and the infinities, which JSON has no numbers for. */
static inline vela__rt__text vela__rt__json_float_view(char* room, double x, bool single) {
    if (!isfinite(x)) {
        return VELA__RT__TEXT("null");
    }
    int len = 0;
    for (int digits = single ? 6 : 15; digits <= (single ? 9 : 17); digits++) {
        len = snprintf(room, VELA__RT__NUMBER_ROOM, "%.*g", digits, x);
        double back = strtod(room, NULL);
        if (single ? (float)back == (float)x : back == x) {
            break;
        }
    }
    return (vela__rt__text){ room, (int64_t)len };
}

static inline void vela__rt__put_json_float(vela__rt__writer* w, double x, bool single) {
    char room[VELA__RT__NUMBER_ROOM];
    vela__rt__text t = vela__rt__json_float_view(room, x, single);
    vela__rt__put(w, t.bytes, (size_t)t.len);
}

static inline vela__rt__text vela__rt__json_float(vela__rt__region* r, double x) {
    vela__rt__writer w = VELA__RT__BUILDER;
    vela__rt__put_json_float(&w, x, false);
    return vela__rt__built(&w, r);
}

static inline vela__rt__text vela__rt__json_float32(vela__rt__region* r, float x) {
    vela__rt__writer w = VELA__RT__BUILDER;
    vela__rt__put_json_float(&w, x, true);
    return vela__rt__built(&w, r);
}

static inline vela__rt__text vela__rt__json_text(vela__rt__region* r, vela__rt__text x) {
    vela__rt__writer w = VELA__RT__BUILDER;
    vela__rt__put_json_text(&w, x);
    return vela__rt__built(&w, r);
}

static inline void vela__rt__put_json_char(vela__rt__writer* w, uint8_t c) {
    char buf[1] = { (char)c };
    vela__rt__text t = { .bytes = buf, .len = 1 };
    vela__rt__put_json_text(w, t);
}

static inline vela__rt__text vela__rt__json_char(vela__rt__region* r, uint8_t c) {
    char buf[1] = { (char)c };
    vela__rt__text t = { .bytes = buf, .len = 1 };
    return vela__rt__json_text(r, t);
}

/* Iterative worklist + address table preserves aliasing and cycles without recursing on C stack. */

struct vela__rt__copy;

typedef void vela__rt__fix(struct vela__rt__copy* c, void* value);

typedef struct vela__rt__copied {
    const void* from;
    void* to;
} vela__rt__copied;

typedef struct vela__rt__pending {
    void* value;
    vela__rt__fix* fix;
} vela__rt__pending;

typedef struct vela__rt__copy {
    vela__rt__region* r;
    /* Open-addressed table; NULL `from` marks empty slots and capacity is 0 or a power of two. */
    vela__rt__copied* table;
    size_t capacity;
    size_t count;
    vela__rt__pending* pending;
    size_t waiting;
    size_t room;
} vela__rt__copy;

static inline vela__rt__text vela__rt__copy_text(vela__rt__copy* c, vela__rt__text text) {
    char* copy = vela__rt__region_alloc(c->r, (size_t)text.len, 1);
    if (text.len > 0) {
        memcpy(copy, text.bytes, (size_t)text.len);
    }
    return (vela__rt__text){ copy, text.len };
}

static inline void* vela__rt__copy_items(vela__rt__copy* c, const void* items, int64_t len, size_t size, size_t align) {
    if (len == 0) {
        return vela__rt__no_items();
    }
    /* Existing slice guarantees len * size cannot overflow size_t. */
    void* copy = vela__rt__region_alloc(c->r, (size_t)len * size, align);
    memcpy(copy, items, (size_t)len * size);
    return copy;
}

static inline size_t vela__rt__copy_slot(const void* from, size_t capacity) {
    uint64_t hash = (uint64_t)(uintptr_t)from * UINT64_C(0x9E3779B97F4A7C15);
    return (size_t)(hash >> 32) & (capacity - 1);
}

static inline void vela__rt__copy_grow(vela__rt__copy* c) {
    size_t capacity = c->capacity == 0 ? 64 : c->capacity;
    if (c->capacity != 0) {
        if (capacity > SIZE_MAX / 2 / sizeof(vela__rt__copied)) {
            vela__rt__out_of_memory();
        }
        capacity *= 2;
    }
    vela__rt__copied* table = calloc(capacity, sizeof(vela__rt__copied));
    if (table == NULL) {
        vela__rt__out_of_memory();
    }
    for (size_t i = 0; i < c->capacity; i++) {
        if (c->table[i].from != NULL) {
            size_t slot = vela__rt__copy_slot(c->table[i].from, capacity);
            while (table[slot].from != NULL) {
                slot = (slot + 1) & (capacity - 1);
            }
            table[slot] = c->table[i];
        }
    }
    free(c->table);
    c->table = table;
    c->capacity = capacity;
}

static inline void* vela__rt__copy_pointer(vela__rt__copy* c, const void* from, size_t size, size_t align, vela__rt__fix* fix) {
    if (from == NULL) {
        return NULL;
    }
    if (2 * (c->count + 1) > c->capacity) {
        vela__rt__copy_grow(c);
    }
    size_t slot = vela__rt__copy_slot(from, c->capacity);
    while (c->table[slot].from != NULL) {
        if (c->table[slot].from == from) {
            return c->table[slot].to;
        }
        slot = (slot + 1) & (c->capacity - 1);
    }
    void* to = vela__rt__region_alloc(c->r, size, align);
    memcpy(to, from, size);
    c->table[slot] = (vela__rt__copied){ .from = from, .to = to };
    c->count++;
    if (fix != NULL) {
        if (c->waiting == c->room) {
            size_t room = c->room == 0 ? 64 : c->room;
            if (c->room != 0) {
                if (room > SIZE_MAX / 2 / sizeof(vela__rt__pending)) {
                    vela__rt__out_of_memory();
                }
                room *= 2;
            }
            vela__rt__pending* pending = realloc(c->pending, room * sizeof(vela__rt__pending));
            if (pending == NULL) {
                vela__rt__out_of_memory();
            }
            c->pending = pending;
            c->room = room;
        }
        c->pending[c->waiting++] = (vela__rt__pending){ .value = to, .fix = fix };
    }
    return to;
}

static inline void vela__rt__copy_finish(vela__rt__copy* c) {
    while (c->waiting > 0) {
        vela__rt__pending next = c->pending[--c->waiting];
        next.fix(c, next.value);
    }
    free(c->table);
    free(c->pending);
}

/* Flushes stdout before prompting; strips CR/LF and preserves interior NUL bytes. */
static inline bool vela__rt__read_line(vela__rt__text prompt, char** bytes, size_t* size) {
    fwrite(prompt.bytes, 1, (size_t)prompt.len, stdout);
    fflush(stdout);
    size_t capacity = 128;
    size_t held = 0;
    char* buffer = malloc(capacity);
    if (buffer == NULL) {
        vela__rt__out_of_memory();
    }
    int c;
    while ((c = getchar()) != EOF && c != '\n') {
        if (held + 1 == capacity) {
            if (capacity > SIZE_MAX / 2) {
                free(buffer);
                vela__rt__out_of_memory();
            }
            char* bigger = realloc(buffer, capacity * 2);
            if (bigger == NULL) {
                free(buffer);
                vela__rt__out_of_memory();
            }
            buffer = bigger;
            capacity *= 2;
        }
        buffer[held++] = (char)c;
    }
    if (c == EOF && held == 0) {
        free(buffer);
        return false;
    }
    if (held > 0 && buffer[held - 1] == '\r') {
        held--;
    }
    *bytes = buffer;
    *size = held;
    return true;
}

static inline bool vela__rt__scan(vela__rt__region* r, vela__rt__text prompt, vela__rt__text* line) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return false;
    }
    char* bytes = vela__rt__region_alloc(r, size, 1);
    memcpy(bytes, buffer, size);
    free(buffer);
    *line = (vela__rt__text){ bytes, (int64_t)size };
    return true;
}

static inline int vela__rt__int_of_input(vela__rt__text prompt, int64_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__int_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__unit_of_text(vela__rt__text t, uint8_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < 0 || whole > 255) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (uint8_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__unit_of_input(vela__rt__text prompt, uint8_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__unit_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__float_of_input(vela__rt__text prompt, double* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__float_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__int8_of_text(vela__rt__text t, int8_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < -128 || whole > 127) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (int8_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__int8_of_input(vela__rt__text prompt, int8_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__int8_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__int16_of_text(vela__rt__text t, int16_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < -32768 || whole > 32767) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (int16_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__int16_of_input(vela__rt__text prompt, int16_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__int16_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__int32_of_text(vela__rt__text t, int32_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < -2147483648LL || whole > 2147483647LL) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (int32_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__int32_of_input(vela__rt__text prompt, int32_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__int32_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__uint16_of_text(vela__rt__text t, uint16_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < 0 || whole > 65535) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (uint16_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__uint16_of_input(vela__rt__text prompt, uint16_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__uint16_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__uint32_of_text(vela__rt__text t, uint32_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < 0 || whole > 4294967295LL) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (uint32_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__uint32_of_input(vela__rt__text prompt, uint32_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__uint32_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__uint64_of_text(vela__rt__text t, uint64_t* value) {
    int64_t whole = 0;
    int read = vela__rt__int_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    if (whole < 0) {
        return VELA__RT__READ_TOO_LARGE;
    }
    *value = (uint64_t)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__uint64_of_input(vela__rt__text prompt, uint64_t* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__uint64_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

static inline int vela__rt__float32_of_text(vela__rt__text t, float* value) {
    double whole = 0.0;
    int read = vela__rt__float_of_text(t, &whole);
    if (read != VELA__RT__READ_OK) {
        return read;
    }
    *value = (float)whole;
    return VELA__RT__READ_OK;
}

static inline int vela__rt__float32_of_input(vela__rt__text prompt, float* value) {
    char* buffer = NULL;
    size_t size = 0;
    if (!vela__rt__read_line(prompt, &buffer, &size)) {
        return VELA__RT__READ_ENDED;
    }
    int read = vela__rt__float32_of_text((vela__rt__text){ buffer, (int64_t)size }, value);
    free(buffer);
    return read;
}

/* Caller-freed NUL-terminated copy; returns NULL if path contains interior '\0'. */
static inline char* vela__rt__path_of(vela__rt__text path) {
    if (memchr(path.bytes, '\0', (size_t)path.len) != NULL) {
        return NULL;
    }
    char* name = malloc((size_t)path.len + 1);
    if (name == NULL) {
        vela__rt__out_of_memory();
    }
    memcpy(name, path.bytes, (size_t)path.len);
    name[path.len] = '\0';
    return name;
}

/* C11 lacks stat; read one byte because fopen succeeds on directories on some systems. */
static inline bool vela__rt__files_path_exists(vela__rt__text path) {
    char* name = vela__rt__path_of(path);
    if (name == NULL) {
        return false;
    }
    FILE* file = fopen(name, "rb");
    free(name);
    if (file == NULL) {
        return false;
    }
    bool readable = getc(file) != EOF || !ferror(file);
    fclose(file);
    return readable;
}

/* Handles pack slot index + generation so reused or region-closed handles report NotOpen. */
enum {
    VELA__RT__FILE_OK = 0,
    VELA__RT__FILE_NOT_FOUND = 1,
    VELA__RT__FILE_NOT_ALLOWED = 2,
    VELA__RT__FILE_ALREADY_OPEN = 3,
    VELA__RT__FILE_NOT_OPEN = 4,
    VELA__RT__FILE_WRONG_MODE = 5,
    VELA__RT__FILE_FAILED = 6,
    VELA__RT__FILE_ENDED = 7
};

/* Tracks last I/O mode because C stdio requires a seek when switching between read and write. */
enum { VELA__RT__MODE_READ, VELA__RT__MODE_WRITE, VELA__RT__MODE_APPEND, VELA__RT__MODE_READ_WRITE };
enum { VELA__RT__DID_NOTHING, VELA__RT__DID_READ, VELA__RT__DID_WRITE };

typedef struct vela__rt__file_slot {
    FILE* file;
    uint32_t generation;
    int mode;
    int did;
} vela__rt__file_slot;

typedef struct vela__rt__file_table {
    vela__rt__file_slot* slots;
    size_t count;
    int64_t status;
} vela__rt__file_table;

/* Function-local static avoids an unused global in programs not using std/files. */
static inline vela__rt__file_table* vela__rt__files(void) {
    static vela__rt__file_table table = { NULL, 0, VELA__RT__FILE_OK };
    return &table;
}

static inline int64_t vela__rt__files_status(void) {
    return vela__rt__files()->status;
}

static inline int64_t vela__rt__files_done(int64_t status) {
    vela__rt__files()->status = status;
    return status;
}

static inline vela__rt__file_slot* vela__rt__file_of(int64_t handle) {
    if (handle < 0) {
        return NULL;
    }
    vela__rt__file_table* table = vela__rt__files();
    size_t index = (size_t)(handle & 0xffffffff);
    uint32_t generation = (uint32_t)(handle >> 32);
    if (index >= table->count) {
        return NULL;
    }
    vela__rt__file_slot* slot = &table->slots[index];
    return slot->file != NULL && slot->generation == generation ? slot : NULL;
}

/* Uses POSIX ENOENT/EACCES when available with a C11 fallback. */
static inline int64_t vela__rt__files_why(int error) {
#ifdef ENOENT
    if (error == ENOENT) {
        return VELA__RT__FILE_NOT_FOUND;
    }
#endif
#ifdef EACCES
    if (error == EACCES) {
        return VELA__RT__FILE_NOT_ALLOWED;
    }
#endif
#ifdef EPERM
    if (error == EPERM) {
        return VELA__RT__FILE_NOT_ALLOWED;
    }
#endif
    (void)error;
    return VELA__RT__FILE_FAILED;
}

static inline int64_t vela__rt__files_open(vela__rt__text path, int64_t mode, vela__rt__region* within) {
    static const char* const how[] = { "rb", "wb", "ab", "r+b" };
    char* name = vela__rt__path_of(path);
    if (name == NULL || mode < 0 || mode > VELA__RT__MODE_READ_WRITE) {
        free(name);
        return -vela__rt__files_done(VELA__RT__FILE_NOT_FOUND);
    }
    errno = 0;
    FILE* file = fopen(name, how[mode]);
    int error = errno;
    free(name);
    if (file == NULL) {
        return -vela__rt__files_done(vela__rt__files_why(error));
    }
    if (mode == VELA__RT__MODE_READ || mode == VELA__RT__MODE_READ_WRITE) {
        int c = getc(file);
        if (c == EOF && ferror(file)) {
            fclose(file);
            return -vela__rt__files_done(VELA__RT__FILE_NOT_FOUND);
        }
        if (c != EOF) {
            ungetc(c, file);
        }
    }
    vela__rt__file_table* table = vela__rt__files();
    size_t index = 0;
    while (index < table->count && table->slots[index].file != NULL) {
        index++;
    }
    if (index == table->count) {
        size_t count = table->count == 0 ? 8 : table->count * 2;
        if (count > 0xffffffffu || count > SIZE_MAX / sizeof(vela__rt__file_slot)) {
            vela__rt__out_of_memory();
        }
        vela__rt__file_slot* slots = realloc(table->slots, count * sizeof(vela__rt__file_slot));
        if (slots == NULL) {
            vela__rt__out_of_memory();
        }
        for (size_t i = table->count; i < count; i++) {
            slots[i] = (vela__rt__file_slot){ NULL, 0, 0, VELA__RT__DID_NOTHING };
        }
        table->slots = slots;
        table->count = count;
    }
    vela__rt__file_slot* slot = &table->slots[index];
    slot->file = file;
    slot->generation++;
    slot->mode = (int)mode;
    slot->did = VELA__RT__DID_NOTHING;
    int64_t handle = (int64_t)(((uint64_t)slot->generation << 32) | (uint64_t)index);
    vela__rt__closer* closer = vela__rt__region_alloc(within, sizeof(vela__rt__closer), _Alignof(vela__rt__closer));
    closer->handle = handle;
    closer->close_fn = NULL;
    closer->next = within->closers;
    within->closers = closer;
    vela__rt__files_done(VELA__RT__FILE_OK);
    return handle;
}

static inline bool vela__rt__files_is_open(int64_t handle) {
    return vela__rt__file_of(handle) != NULL;
}

static inline int64_t vela__rt__files_close(int64_t handle) {
    vela__rt__file_slot* slot = vela__rt__file_of(handle);
    if (slot == NULL) {
        return vela__rt__files_done(VELA__RT__FILE_NOT_OPEN);
    }
    bool closed = fclose(slot->file) == 0;
    slot->file = NULL;
    return vela__rt__files_done(closed ? VELA__RT__FILE_OK : VELA__RT__FILE_FAILED);
}

/* Seeks between read/write per C stdio rules and flushes stdout in case the handle aliases /dev/stdout. */
static inline vela__rt__file_slot* vela__rt__files_ready(int64_t handle, bool reading) {
    fflush(stdout);
    vela__rt__file_slot* slot = vela__rt__file_of(handle);
    if (slot == NULL) {
        vela__rt__files_done(VELA__RT__FILE_NOT_OPEN);
        return NULL;
    }
    bool can = reading ? slot->mode == VELA__RT__MODE_READ || slot->mode == VELA__RT__MODE_READ_WRITE : slot->mode != VELA__RT__MODE_READ;
    if (!can) {
        vela__rt__files_done(VELA__RT__FILE_WRONG_MODE);
        return NULL;
    }
    int doing = reading ? VELA__RT__DID_READ : VELA__RT__DID_WRITE;
    if (slot->did != VELA__RT__DID_NOTHING && slot->did != doing) {
        fseek(slot->file, 0, SEEK_CUR);
    }
    slot->did = doing;
    vela__rt__files_done(VELA__RT__FILE_OK);
    return slot;
}

static inline bool vela__rt__files_gather(FILE* file, bool line, char** bytes, size_t* size) {
    size_t capacity = 128;
    size_t held = 0;
    char* buffer = malloc(capacity);
    if (buffer == NULL) {
        vela__rt__out_of_memory();
    }
    int c = EOF;
    bool any = false;
    while ((c = getc(file)) != EOF) {
        any = true;
        if (line && c == '\n') {
            break;
        }
        if (held + 1 == capacity) {
            if (capacity > SIZE_MAX / 2) {
                free(buffer);
                vela__rt__out_of_memory();
            }
            char* bigger = realloc(buffer, capacity * 2);
            if (bigger == NULL) {
                free(buffer);
                vela__rt__out_of_memory();
            }
            buffer = bigger;
            capacity *= 2;
        }
        buffer[held++] = (char)c;
    }
    if (line && held > 0 && buffer[held - 1] == '\r') {
        held--;
    }
    *bytes = buffer;
    *size = held;
    return any;
}

static inline vela__rt__text vela__rt__files_read(int64_t handle, vela__rt__region* into) {
    vela__rt__file_slot* slot = vela__rt__files_ready(handle, true);
    if (slot == NULL) {
        return (vela__rt__text){ "", 0 };
    }
    char* bytes = NULL;
    size_t size = 0;
    vela__rt__files_gather(slot->file, false, &bytes, &size);
    vela__rt__text text = vela__rt__text_in(into, bytes, size);
    free(bytes);
    if (ferror(slot->file)) {
        clearerr(slot->file);
        vela__rt__files_done(VELA__RT__FILE_FAILED);
    }
    return text;
}

static inline vela__rt__text vela__rt__files_line(int64_t handle, vela__rt__region* into) {
    vela__rt__file_slot* slot = vela__rt__files_ready(handle, true);
    if (slot == NULL) {
        return (vela__rt__text){ "", 0 };
    }
    char* bytes = NULL;
    size_t size = 0;
    bool any = vela__rt__files_gather(slot->file, true, &bytes, &size);
    vela__rt__text text = vela__rt__text_in(into, bytes, size);
    free(bytes);
    if (ferror(slot->file)) {
        clearerr(slot->file);
        vela__rt__files_done(VELA__RT__FILE_FAILED);
    } else if (!any) {
        vela__rt__files_done(VELA__RT__FILE_ENDED);
    }
    return text;
}

static inline int64_t vela__rt__files_write(int64_t handle, vela__rt__text text) {
    vela__rt__file_slot* slot = vela__rt__files_ready(handle, false);
    if (slot == NULL) {
        return vela__rt__files_status();
    }
    size_t wrote = fwrite(text.bytes, 1, (size_t)text.len, slot->file);
    return vela__rt__files_done(wrote == (size_t)text.len ? VELA__RT__FILE_OK : VELA__RT__FILE_FAILED);
}

static inline bool vela__rt__files_ended(int64_t handle) {
    fflush(stdout);
    vela__rt__file_slot* slot = vela__rt__file_of(handle);
    if (slot == NULL || (slot->mode != VELA__RT__MODE_READ && slot->mode != VELA__RT__MODE_READ_WRITE)) {
        return true;
    }
    if (slot->did == VELA__RT__DID_WRITE) {
        fseek(slot->file, 0, SEEK_CUR);
        slot->did = VELA__RT__DID_READ;
    }
    int c = getc(slot->file);
    if (c == EOF) {
        clearerr(slot->file);
        return true;
    }
    ungetc(c, slot->file);
    return false;
}

static inline int64_t vela__rt__files_size(vela__rt__text path, int64_t handle) {
    vela__rt__file_slot* slot = vela__rt__file_of(handle);
    FILE* file = slot != NULL ? slot->file : NULL;
    long at = 0;
    if (file != NULL) {
        fflush(file);
        at = ftell(file);
    } else {
        char* name = vela__rt__path_of(path);
        if (name == NULL) {
            return -vela__rt__files_done(VELA__RT__FILE_NOT_FOUND);
        }
        errno = 0;
        file = fopen(name, "rb");
        int error = errno;
        free(name);
        if (file == NULL) {
            return -vela__rt__files_done(vela__rt__files_why(error));
        }
    }
    long size = fseek(file, 0, SEEK_END) == 0 ? ftell(file) : -1;
    if (slot != NULL) {
        fseek(file, at, SEEK_SET);
    } else {
        fclose(file);
    }
    if (size < 0) {
        return -vela__rt__files_done(VELA__RT__FILE_FAILED);
    }
    vela__rt__files_done(VELA__RT__FILE_OK);
    return (int64_t)size;
}

static inline int64_t vela__rt__files_rename(vela__rt__text from, vela__rt__text to) {
    char* old = vela__rt__path_of(from);
    char* new_name = vela__rt__path_of(to);
    if (old == NULL || new_name == NULL) {
        free(old);
        free(new_name);
        return vela__rt__files_done(VELA__RT__FILE_NOT_FOUND);
    }
    errno = 0;
    int failed = rename(old, new_name);
    int error = errno;
    free(old);
    free(new_name);
    return vela__rt__files_done(failed == 0 ? VELA__RT__FILE_OK : vela__rt__files_why(error));
}

static inline int64_t vela__rt__files_remove(vela__rt__text path) {
    char* name = vela__rt__path_of(path);
    if (name == NULL) {
        return vela__rt__files_done(VELA__RT__FILE_NOT_FOUND);
    }
    errno = 0;
    int failed = remove(name);
    int error = errno;
    free(name);
    return vela__rt__files_done(failed == 0 ? VELA__RT__FILE_OK : vela__rt__files_why(error));
}

/* Invoke closers before freeing chunks because closer records live inside the arena. */
static inline void vela__rt__region_free(vela__rt__region* r) {
    for (vela__rt__closer* closer = r->closers; closer != NULL; closer = closer->next) {
        if (closer->close_fn != NULL) {
            closer->close_fn(closer->handle);
        } else {
            vela__rt__file_slot* slot = vela__rt__file_of(closer->handle);
            if (slot != NULL) {
                fclose(slot->file);
                slot->file = NULL;
            }
        }
    }
    r->closers = NULL;
    vela__rt__chunk* chunk = r->newest;
    while (chunk != NULL) {
        vela__rt__chunk* previous = chunk->previous;
        free(chunk);
        chunk = previous;
    }
    r->newest = NULL;
}

/* Unreachable sentinel silencing missing-return warnings in non-void C functions. */
static inline _Noreturn void vela__rt__missing_send(const char* task) {
    fflush(stdout);
    fprintf(stderr, "vela: task '%s' finished without sending a value\n", task);
    abort();
}

/* Held as 8 64-bit unsigned limbs in two's complement, least significant first. */

typedef struct vela__rt__int512 {
    uint64_t limbs[8];
} vela__rt__int512;

#if defined(__SIZEOF_INT128__)
__extension__ typedef unsigned __int128 vela__rt__u128;
#endif

#define vela__rt__int512_cast_in(b) \
    __extension__ ({ \
        vela__rt__int512 __cin; \
        memcpy(&__cin, &(b), sizeof(__cin)); \
        __cin; \
    })
#define vela__rt__int512_cast_out(expr) \
    __extension__ ({ \
        vela__rt__int512 __val = (expr); \
        vela_std__1__int512__0__Int512 __ret; \
        memcpy(&__ret, &__val, sizeof(__ret)); \
        __ret; \
    })

static inline bool vela__rt__int512_is_neg(vela__rt__int512 a) {
    return (a.limbs[7] & 0x8000000000000000ULL) != 0;
}

static inline bool vela__rt__int512_is_zero(vela__rt__int512 a) {
    for (int i = 0; i < 8; i++) {
        if (a.limbs[i] != 0) return false;
    }
    return true;
}

static inline bool vela__rt__int512_is_min(vela__rt__int512 a) {
    if (a.limbs[7] != 0x8000000000000000ULL) return false;
    for (int i = 0; i < 7; i++) {
        if (a.limbs[i] != 0) return false;
    }
    return true;
}

static inline int64_t vela__rt__int512_cmp_impl(vela__rt__int512 a, vela__rt__int512 b) {
    bool aneg = vela__rt__int512_is_neg(a);
    bool bneg = vela__rt__int512_is_neg(b);
    if (aneg != bneg) return aneg ? -1 : 1;
    for (int i = 7; i >= 0; i--) {
        if (a.limbs[i] < b.limbs[i]) return -1;
        if (a.limbs[i] > b.limbs[i]) return 1;
    }
    return 0;
}

static inline int vela__rt__u512_cmp(vela__rt__int512 a, vela__rt__int512 b) {
    for (int i = 7; i >= 0; i--) {
        if (a.limbs[i] < b.limbs[i]) return -1;
        if (a.limbs[i] > b.limbs[i]) return 1;
    }
    return 0;
}

static inline bool vela__rt__u512_add(vela__rt__int512 a, vela__rt__int512 b, vela__rt__int512* out) {
    uint64_t carry = 0;
    for (int i = 0; i < 8; i++) {
#if defined(__SIZEOF_INT128__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
        vela__rt__u128 sum = (vela__rt__u128)a.limbs[i] + b.limbs[i] + carry;
        out->limbs[i] = (uint64_t)sum;
        carry = (uint64_t)(sum >> 64);
#else
        uint64_t ai = a.limbs[i], bi = b.limbs[i];
        uint64_t s1 = ai + carry;
        uint64_t c1 = (s1 < ai) ? 1 : 0;
        uint64_t s2 = s1 + bi;
        uint64_t c2 = (s2 < s1) ? 1 : 0;
        out->limbs[i] = s2;
        carry = c1 + c2;
#endif
    }
    return carry != 0;
}

static inline bool vela__rt__u512_sub(vela__rt__int512 a, vela__rt__int512 b, vela__rt__int512* out) {
    uint64_t borrow = 0;
    for (int i = 0; i < 8; i++) {
#if defined(__SIZEOF_INT128__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
        vela__rt__u128 ai = a.limbs[i];
        vela__rt__u128 sub = (vela__rt__u128)b.limbs[i] + borrow;
        out->limbs[i] = (uint64_t)(ai - sub);
        borrow = (ai < sub) ? 1 : 0;
#else
        uint64_t ai = a.limbs[i], bi = b.limbs[i];
        uint64_t d1 = ai - borrow;
        uint64_t b1 = (ai < borrow) ? 1 : 0;
        uint64_t d2 = d1 - bi;
        uint64_t b2 = (d1 < bi) ? 1 : 0;
        out->limbs[i] = d2;
        borrow = b1 + b2;
#endif
    }
    return borrow != 0;
}

static inline vela__rt__int512 vela__rt__u512_neg(vela__rt__int512 a) {
    vela__rt__int512 zero = {{0}};
    vela__rt__int512 res;
    vela__rt__u512_sub(zero, a, &res);
    return res;
}

// The low 512 bits of an unsigned product, with no overflow check: text
// parsing bounds its values first (see `vela__rt__int512_of_text`), where a
// checked multiply would trap on the sign bit of a value still in range.
static inline void vela__rt__u512_mul(vela__rt__int512 a, vela__rt__int512 b, vela__rt__int512* out) {
    uint64_t prod[16] = {0};
    for (int i = 0; i < 8; i++) {
        if (a.limbs[i] == 0) continue;
        uint64_t carry = 0;
        for (int j = 0; j < 8; j++) {
#if defined(__SIZEOF_INT128__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
            vela__rt__u128 cur = (vela__rt__u128)a.limbs[i] * b.limbs[j] + prod[i + j] + carry;
            prod[i + j] = (uint64_t)cur;
            carry = (uint64_t)(cur >> 64);
#else
            uint64_t u_lo = a.limbs[i] & 0xFFFFFFFFULL, u_hi = a.limbs[i] >> 32;
            uint64_t v_lo = b.limbs[j] & 0xFFFFFFFFULL, v_hi = b.limbs[j] >> 32;
            uint64_t p0 = u_lo * v_lo;
            uint64_t p1 = u_lo * v_hi;
            uint64_t p2 = u_hi * v_lo;
            uint64_t p3 = u_hi * v_hi;
            uint64_t lo_sum = (prod[i + j] & 0xFFFFFFFFULL) + (p0 & 0xFFFFFFFFULL) + (carry & 0xFFFFFFFFULL);
            uint64_t mid_sum = (lo_sum >> 32) + (prod[i + j] >> 32) + (p0 >> 32) + (p1 & 0xFFFFFFFFULL) + (p2 & 0xFFFFFFFFULL) + (carry >> 32);
            uint64_t hi_sum = (mid_sum >> 32) + (p1 >> 32) + (p2 >> 32) + p3;
            prod[i + j] = (lo_sum & 0xFFFFFFFFULL) | (mid_sum << 32);
            carry = hi_sum;
#endif
        }
        prod[i + 8] += carry;
    }
    for (int i = 0; i < 8; i++) out->limbs[i] = prod[i];
}

static inline vela__rt__int512 vela__rt__int512_add_impl(vela__rt__int512 a, vela__rt__int512 b) {
    vela__rt__int512 c;
    vela__rt__u512_add(a, b, &c);
    bool ov = (~(a.limbs[7] ^ b.limbs[7]) & (a.limbs[7] ^ c.limbs[7]) & 0x8000000000000000ULL) != 0;
    if (ov) {
        vela__rt__int_trap("integer overflow", "Int512.plus");
    }
    return c;
}

static inline vela__rt__int512 vela__rt__int512_sub_impl(vela__rt__int512 a, vela__rt__int512 b) {
    vela__rt__int512 c;
    vela__rt__u512_sub(a, b, &c);
    bool ov = ((a.limbs[7] ^ b.limbs[7]) & (a.limbs[7] ^ c.limbs[7]) & 0x8000000000000000ULL) != 0;
    if (ov) {
        vela__rt__int_trap("integer overflow", "Int512.minus");
    }
    return c;
}

static inline vela__rt__int512 vela__rt__int512_neg_impl(vela__rt__int512 a) {
    if (vela__rt__int512_is_min(a)) {
        vela__rt__int_trap("integer overflow", "Int512.neg");
    }
    return vela__rt__u512_neg(a);
}

static inline vela__rt__int512 vela__rt__int512_abs_impl(vela__rt__int512 a) {
    if (vela__rt__int512_is_min(a)) {
        vela__rt__int_trap("integer overflow", "Int512.abs");
    }
    return vela__rt__int512_is_neg(a) ? vela__rt__u512_neg(a) : a;
}

static inline vela__rt__int512 vela__rt__int512_mul_impl(vela__rt__int512 a, vela__rt__int512 b) {
    if (vela__rt__int512_is_zero(a) || vela__rt__int512_is_zero(b)) {
        vela__rt__int512 z = {{0}};
        return z;
    }
    bool amin = vela__rt__int512_is_min(a);
    bool bmin = vela__rt__int512_is_min(b);
    if (amin || bmin) {
        vela__rt__int512 other = amin ? b : a;
        if (other.limbs[0] == 1) {
            bool all_zero = true;
            for (int i = 1; i < 8; i++) if (other.limbs[i] != 0) all_zero = false;
            if (all_zero) {
                vela__rt__int512 m = {{0}};
                m.limbs[7] = 0x8000000000000000ULL;
                return m;
            }
        }
        vela__rt__int_trap("integer overflow", "Int512.times");
    }

    bool aneg = vela__rt__int512_is_neg(a);
    bool bneg = vela__rt__int512_is_neg(b);
    bool res_neg = aneg ^ bneg;

    vela__rt__int512 ua = aneg ? vela__rt__u512_neg(a) : a;
    vela__rt__int512 ub = bneg ? vela__rt__u512_neg(b) : b;

    uint64_t prod[16] = {0};
    for (int i = 0; i < 8; i++) {
        if (ua.limbs[i] == 0) continue;
        uint64_t carry = 0;
        for (int j = 0; j < 8; j++) {
#if defined(__SIZEOF_INT128__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
            vela__rt__u128 cur = (vela__rt__u128)ua.limbs[i] * ub.limbs[j] + prod[i + j] + carry;
            prod[i + j] = (uint64_t)cur;
            carry = (uint64_t)(cur >> 64);
#else
            uint64_t u_lo = ua.limbs[i] & 0xFFFFFFFFULL, u_hi = ua.limbs[i] >> 32;
            uint64_t v_lo = ub.limbs[j] & 0xFFFFFFFFULL, v_hi = ub.limbs[j] >> 32;
            uint64_t p0 = u_lo * v_lo;
            uint64_t p1 = u_lo * v_hi;
            uint64_t p2 = u_hi * v_lo;
            uint64_t p3 = u_hi * v_hi;
            uint64_t lo_sum = (prod[i + j] & 0xFFFFFFFFULL) + (p0 & 0xFFFFFFFFULL) + (carry & 0xFFFFFFFFULL);
            uint64_t mid_sum = (lo_sum >> 32) + (prod[i + j] >> 32) + (p0 >> 32) + (p1 & 0xFFFFFFFFULL) + (p2 & 0xFFFFFFFFULL) + (carry >> 32);
            uint64_t hi_sum = (mid_sum >> 32) + (p1 >> 32) + (p2 >> 32) + p3;
            prod[i + j] = (lo_sum & 0xFFFFFFFFULL) | (mid_sum << 32);
            carry = hi_sum;
#endif
        }
        prod[i + 8] += carry;
    }

    bool ov = false;
    for (int i = 8; i < 16; i++) {
        if (prod[i] != 0) { ov = true; break; }
    }
    vela__rt__int512 res;
    for (int i = 0; i < 8; i++) res.limbs[i] = prod[i];

    if (!ov) {
        if (!res_neg) {
            if ((res.limbs[7] & 0x8000000000000000ULL) != 0) ov = true;
        } else {
            if (vela__rt__int512_is_min(res)) {
                ov = false;
            } else if ((res.limbs[7] & 0x8000000000000000ULL) != 0) {
                ov = true;
            }
        }
    }

    if (ov) {
        vela__rt__int_trap("integer overflow", "Int512.times");
    }
    if (res_neg) res = vela__rt__u512_neg(res);
    return res;
}

static inline void vela__rt__u512_divrem(vela__rt__int512 u, vela__rt__int512 v, vela__rt__int512* q, vela__rt__int512* r) {
    vela__rt__int512 zero = {{0}};
    if (vela__rt__u512_cmp(u, v) < 0) {
        if (q) *q = zero;
        if (r) *r = u;
        return;
    }
    bool v_single = true;
    for (int i = 1; i < 8; i++) {
        if (v.limbs[i] != 0) { v_single = false; break; }
    }
    if (v_single) {
        uint64_t rem = 0;
        uint64_t div = v.limbs[0];
        vela__rt__int512 quot = {{0}};
        for (int i = 7; i >= 0; i--) {
#if defined(__SIZEOF_INT128__) && !defined(VELA__RT__PORTABLE_ARITHMETIC)
            vela__rt__u128 cur = ((vela__rt__u128)rem << 64) | u.limbs[i];
            quot.limbs[i] = (uint64_t)(cur / div);
            rem = (uint64_t)(cur % div);
#else
            // Without 128-bit integers, divide the 96-bit (rem, limb) by div
            // 32 quotient bits at a time: rem << 32 alone would lose bits when
            // div is large, so the running remainder is shifted in single bits.
            uint64_t hi = u.limbs[i] >> 32;
            uint64_t lo = u.limbs[i] & 0xFFFFFFFFULL;
            uint64_t q1 = 0;
            for (int k = 31; k >= 0; k--) {
                uint64_t top = rem >> 63;
                rem = (rem << 1) | ((hi >> k) & 1);
                if (top || rem >= div) {
                    rem -= div;
                    q1 |= 1ULL << k;
                }
            }
            uint64_t q2 = 0;
            for (int k = 31; k >= 0; k--) {
                uint64_t top = rem >> 63;
                rem = (rem << 1) | ((lo >> k) & 1);
                if (top || rem >= div) {
                    rem -= div;
                    q2 |= 1ULL << k;
                }
            }
            quot.limbs[i] = (q1 << 32) | q2;
#endif
        }
        if (q) *q = quot;
        if (r) {
            vela__rt__int512 rem_val = {{0}};
            rem_val.limbs[0] = rem;
            *r = rem_val;
        }
        return;
    }

    int u_bits = 0;
    for (int i = 7; i >= 0; i--) {
        if (u.limbs[i] != 0) {
            u_bits = i * 64 + (64 - __builtin_clzll(u.limbs[i]));
            break;
        }
    }
    vela__rt__int512 quot = {{0}};
    vela__rt__int512 rem = {{0}};
    for (int bit = u_bits - 1; bit >= 0; bit--) {
        uint64_t c = 0;
        for (int i = 0; i < 8; i++) {
            uint64_t next_c = rem.limbs[i] >> 63;
            rem.limbs[i] = (rem.limbs[i] << 1) | c;
            c = next_c;
        }
        int limb_idx = bit / 64;
        int bit_idx = bit % 64;
        if ((u.limbs[limb_idx] >> bit_idx) & 1) {
            rem.limbs[0] |= 1;
        }
        if (vela__rt__u512_cmp(rem, v) >= 0) {
            vela__rt__u512_sub(rem, v, &rem);
            quot.limbs[limb_idx] |= (1ULL << bit_idx);
        }
    }
    if (q) *q = quot;
    if (r) *r = rem;
}

static inline vela__rt__int512 vela__rt__int512_div_impl(vela__rt__int512 a, vela__rt__int512 b) {
    if (vela__rt__int512_is_zero(b)) {
        vela__rt__int_trap("division by zero", "Int512.div");
    }
    if (vela__rt__int512_is_min(a)) {
        bool b_neg1 = true;
        for (int i = 0; i < 8; i++) {
            if (b.limbs[i] != 0xFFFFFFFFFFFFFFFFULL) { b_neg1 = false; break; }
        }
        if (b_neg1) {
            vela__rt__int_trap("integer overflow", "Int512.div");
        }
    }

    bool aneg = vela__rt__int512_is_neg(a);
    bool bneg = vela__rt__int512_is_neg(b);
    bool res_neg = aneg ^ bneg;

    vela__rt__int512 ua = aneg ? vela__rt__u512_neg(a) : a;
    vela__rt__int512 ub = bneg ? vela__rt__u512_neg(b) : b;

    vela__rt__int512 q;
    vela__rt__u512_divrem(ua, ub, &q, NULL);
    if (res_neg) q = vela__rt__u512_neg(q);
    return q;
}

static inline vela__rt__int512 vela__rt__int512_rem_impl(vela__rt__int512 a, vela__rt__int512 b) {
    if (vela__rt__int512_is_zero(b)) {
        vela__rt__int_trap("remainder of division by zero", "Int512.rem");
    }
    bool aneg = vela__rt__int512_is_neg(a);
    bool bneg = vela__rt__int512_is_neg(b);

    vela__rt__int512 ua = aneg ? vela__rt__u512_neg(a) : a;
    vela__rt__int512 ub = bneg ? vela__rt__u512_neg(b) : b;

    vela__rt__int512 r;
    vela__rt__u512_divrem(ua, ub, NULL, &r);
    if (aneg) r = vela__rt__u512_neg(r);
    return r;
}

static inline int64_t vela__rt__int512_to_int64_impl(vela__rt__int512 a) {
    bool neg = vela__rt__int512_is_neg(a);
    if (!neg) {
        for (int i = 1; i < 8; i++) {
            if (a.limbs[i] != 0) vela__rt__int_trap("integer overflow", "Int512.to_int64");
        }
        if (a.limbs[0] > (uint64_t)INT64_MAX) {
            vela__rt__int_trap("integer overflow", "Int512.to_int64");
        }
        return (int64_t)a.limbs[0];
    } else {
        for (int i = 1; i < 8; i++) {
            if (a.limbs[i] != ~0ULL) vela__rt__int_trap("integer overflow", "Int512.to_int64");
        }
        if (a.limbs[0] < (uint64_t)INT64_MIN) {
            vela__rt__int_trap("integer overflow", "Int512.to_int64");
        }
        return (int64_t)a.limbs[0];
    }
}

static inline uint64_t vela__rt__int512_to_uint64_impl(vela__rt__int512 a) {
    if (vela__rt__int512_is_neg(a)) {
        vela__rt__int_trap("integer overflow", "Int512.to_uint64");
    }
    for (int i = 1; i < 8; i++) {
        if (a.limbs[i] != 0) vela__rt__int_trap("integer overflow", "Int512.to_uint64");
    }
    return a.limbs[0];
}

static inline vela__rt__int512 vela__rt__int512_from_int64_impl(int64_t n) {
    vela__rt__int512 r = {{0}};
    r.limbs[0] = (uint64_t)n;
    if (n < 0) {
        for (int i = 1; i < 8; i++) r.limbs[i] = ~0ULL;
    }
    return r;
}

static inline vela__rt__int512 vela__rt__int512_from_uint64_impl(uint64_t n) {
    vela__rt__int512 r = {{0}};
    r.limbs[0] = n;
    return r;
}

static inline vela__rt__int512 vela__rt__int512_of_uint64(uint64_t v) {
    vela__rt__int512 r = {{0}};
    r.limbs[0] = v;
    return r;
}

static inline int vela__rt__int512_of_text(const char* bytes, size_t len, vela__rt__int512* out) {
    size_t i = 0;
    bool negative = false;
    if (len > 0 && bytes[0] == '-') {
        negative = true;
        i = 1;
    } else if (len > 0 && bytes[0] == '+') {
        i = 1;
    }
    if (i >= len) {
        return 1;
    }
    uint64_t base = 10u;
    if (len - i > 2 && bytes[i] == '0') {
        switch (bytes[i + 1]) {
        case 'x': case 'X': base = 16u; break;
        case 'o': case 'O': base = 8u; break;
        case 'b': case 'B': base = 2u; break;
        default: break;
        }
        if (base != 10u) {
            i += 2;
        }
    }
    if (i >= len) {
        return 1;
    }

    vela__rt__int512 limit = {{0}};
    if (negative) {
        limit.limbs[7] = 0x8000000000000000ULL;
    } else {
        for (int k = 0; k < 7; k++) limit.limbs[k] = ~0ULL;
        limit.limbs[7] = 0x7FFFFFFFFFFFFFFFULL;
    }

    vela__rt__int512 mag = {{0}};
    vela__rt__int512 vbase = vela__rt__int512_of_uint64(base);

    for (; i < len; i++) {
        unsigned char c = (unsigned char)bytes[i];
        uint64_t digit = base;
        if (c >= '0' && c <= '9') {
            digit = (uint64_t)(c - '0');
        } else if (c >= 'a' && c <= 'f') {
            digit = (uint64_t)(c - 'a') + 10u;
        } else if (c >= 'A' && c <= 'F') {
            digit = (uint64_t)(c - 'A') + 10u;
        }
        if (digit >= base) {
            return 1;
        }
        vela__rt__int512 vdigit = vela__rt__int512_of_uint64(digit);
        vela__rt__int512 max_prev;
        vela__rt__u512_sub(limit, vdigit, &max_prev);
        vela__rt__int512 max_mag;
        vela__rt__u512_divrem(max_prev, vbase, &max_mag, NULL);
        if (vela__rt__u512_cmp(mag, max_mag) > 0) {
            return 2;
        }
        vela__rt__u512_mul(mag, vbase, &mag);
        vela__rt__u512_add(mag, vdigit, &mag);
    }

    if (negative) {
        *out = vela__rt__u512_neg(mag);
    } else {
        *out = mag;
    }
    return 0;
}

static inline size_t vela__rt__int512_to_str(vela__rt__int512 a, char* buf) {
    if (vela__rt__int512_is_zero(a)) {
        buf[0] = '0';
        buf[1] = '\0';
        return 1;
    }
    bool neg = vela__rt__int512_is_neg(a);
    vela__rt__int512 mag;
    if (neg) {
        mag = vela__rt__u512_neg(a);
    } else {
        mag = a;
    }

    char tmp[200];
    int pos = 0;
    vela__rt__int512 ten19 = vela__rt__int512_of_uint64(10000000000000000000ULL);

    while (!vela__rt__int512_is_zero(mag)) {
        vela__rt__int512 q, r;
        vela__rt__u512_divrem(mag, ten19, &q, &r);
        uint64_t chunk = r.limbs[0];
        mag = q;

        if (vela__rt__int512_is_zero(mag)) {
            while (chunk > 0) {
                tmp[pos++] = (char)('0' + (chunk % 10));
                chunk /= 10;
            }
        } else {
            for (int k = 0; k < 19; k++) {
                tmp[pos++] = (char)('0' + (chunk % 10));
                chunk /= 10;
            }
        }
    }
    if (neg) {
        tmp[pos++] = '-';
    }

    for (int k = 0; k < pos; k++) {
        buf[k] = tmp[pos - 1 - k];
    }
    buf[pos] = '\0';
    return (size_t)pos;
}

static inline int64_t vela__rt__int512_parse_status_impl(vela__rt__text t) {
    vela__rt__int512 val;
    int status = vela__rt__int512_of_text(t.bytes, (size_t)(t.len < 0 ? 0 : t.len), &val);
    return (int64_t)status;
}

static inline vela__rt__int512 vela__rt__int512_parse_value_impl(vela__rt__text t) {
    vela__rt__int512 val = {{0}};
    vela__rt__int512_of_text(t.bytes, (size_t)(t.len < 0 ? 0 : t.len), &val);
    return val;
}

static inline vela__rt__text vela__rt__int512_to_text_impl(vela__rt__int512 a, vela__rt__region* r) {
    char buf[200];
    size_t len = vela__rt__int512_to_str(a, buf);
    return vela__rt__text_in(r, buf, len);
}

#define vela__rt__int512_add(a, b) vela__rt__int512_cast_out(vela__rt__int512_add_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b)))
#define vela__rt__int512_sub(a, b) vela__rt__int512_cast_out(vela__rt__int512_sub_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b)))
#define vela__rt__int512_mul(a, b) vela__rt__int512_cast_out(vela__rt__int512_mul_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b)))
#define vela__rt__int512_div(a, b) vela__rt__int512_cast_out(vela__rt__int512_div_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b)))
#define vela__rt__int512_rem(a, b) vela__rt__int512_cast_out(vela__rt__int512_rem_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b)))
#define vela__rt__int512_neg(a) vela__rt__int512_cast_out(vela__rt__int512_neg_impl(vela__rt__int512_cast_in(a)))
#define vela__rt__int512_abs(a) vela__rt__int512_cast_out(vela__rt__int512_abs_impl(vela__rt__int512_cast_in(a)))
#define vela__rt__int512_cmp(a, b) vela__rt__int512_cmp_impl(vela__rt__int512_cast_in(a), vela__rt__int512_cast_in(b))
#define vela__rt__int512_to_int64(a) vela__rt__int512_to_int64_impl(vela__rt__int512_cast_in(a))
#define vela__rt__int512_to_uint64(a) vela__rt__int512_to_uint64_impl(vela__rt__int512_cast_in(a))
#define vela__rt__int512_from_int64(n) vela__rt__int512_cast_out(vela__rt__int512_from_int64_impl(n))
#define vela__rt__int512_from_uint64(n) vela__rt__int512_cast_out(vela__rt__int512_from_uint64_impl(n))
#define vela__rt__int512_parse_status(t) vela__rt__int512_parse_status_impl(t)
#define vela__rt__int512_parse_value(t) vela__rt__int512_cast_out(vela__rt__int512_parse_value_impl(t))
#define vela__rt__int512_to_text(a, into) vela__rt__int512_to_text_impl(vela__rt__int512_cast_in(a), (into))

/* Socket runtime for std/web non-blocking networking. */

enum {
    VELA__RT__SOCK_OK = 0,
    VELA__RT__SOCK_WOULD_BLOCK = 1,
    VELA__RT__SOCK_CLOSED = 2,
    VELA__RT__SOCK_FAILED = 3,
    VELA__RT__SOCK_INVALID_HANDLE = 4,
    VELA__RT__SOCK_ADDR_IN_USE = 5,
    VELA__RT__SOCK_CONN_REFUSED = 6,
    VELA__RT__SOCK_CONN_RESET = 7,
    VELA__RT__SOCK_TIMEOUT = 8
};

typedef struct vela__rt__socket_slot {
    int fd;
    uint32_t generation;
} vela__rt__socket_slot;

typedef struct vela__rt__socket_table {
    vela__rt__socket_slot* slots;
    size_t count;
    int64_t status;
} vela__rt__socket_table;

static inline vela__rt__socket_table* vela__rt__sockets(void) {
    static vela__rt__socket_table table = { NULL, 0, VELA__RT__SOCK_OK };
    return &table;
}

static inline int64_t vela__rt__web_socket_status(void) {
    return vela__rt__sockets()->status;
}

static inline int64_t vela__rt__sockets_done(int64_t status) {
    vela__rt__sockets()->status = status;
    return status;
}

static inline vela__rt__socket_slot* vela__rt__socket_of(int64_t handle) {
    if (handle < 0) {
        return NULL;
    }
    vela__rt__socket_table* table = vela__rt__sockets();
    size_t index = (size_t)(handle & 0xffffffff);
    uint32_t generation = (uint32_t)(handle >> 32);
    if (index >= table->count) {
        return NULL;
    }
    vela__rt__socket_slot* slot = &table->slots[index];
    return slot->fd >= 0 && slot->generation == generation ? slot : NULL;
}

static inline bool vela__rt__socket_set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0) {
        return false;
    }
    return fcntl(fd, F_SETFL, flags | O_NONBLOCK) >= 0;
}

static inline int64_t vela__rt__web_socket_close(int64_t handle) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    int fd = slot->fd;
    slot->fd = -1;
    int res = close(fd);
    return vela__rt__sockets_done(res == 0 ? VELA__RT__SOCK_OK : VELA__RT__SOCK_FAILED);
}

static inline void vela__rt__web_socket_closer_hook(int64_t handle) {
    vela__rt__web_socket_close(handle);
}

static inline int64_t vela__rt__socket_alloc_slot(int fd, vela__rt__region* within) {
    vela__rt__socket_table* table = vela__rt__sockets();
    size_t index = 0;
    while (index < table->count && table->slots[index].fd >= 0) {
        index++;
    }
    if (index == table->count) {
        size_t count = table->count == 0 ? 8 : table->count * 2;
        if (count > 0xffffffffu || count > SIZE_MAX / sizeof(vela__rt__socket_slot)) {
            vela__rt__out_of_memory();
        }
        vela__rt__socket_slot* slots = realloc(table->slots, count * sizeof(vela__rt__socket_slot));
        if (slots == NULL) {
            vela__rt__out_of_memory();
        }
        for (size_t i = table->count; i < count; i++) {
            slots[i] = (vela__rt__socket_slot){ -1, 0 };
        }
        table->slots = slots;
        table->count = count;
    }
    vela__rt__socket_slot* slot = &table->slots[index];
    slot->fd = fd;
    slot->generation++;
    int64_t handle = (int64_t)(((uint64_t)slot->generation << 32) | (uint64_t)index);
    if (within != NULL) {
        vela__rt__closer* closer = vela__rt__region_alloc(within, sizeof(vela__rt__closer), _Alignof(vela__rt__closer));
        closer->handle = handle;
        closer->close_fn = vela__rt__web_socket_closer_hook;
        closer->next = within->closers;
        within->closers = closer;
    }
    vela__rt__sockets_done(VELA__RT__SOCK_OK);
    return handle;
}

static inline int64_t vela__rt__web_socket_open(vela__rt__region* within) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    int yes = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, (const void*)&yes, sizeof(yes));
#ifdef SO_NOSIGPIPE
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, (const void*)&yes, sizeof(yes));
#endif
    if (!vela__rt__socket_set_nonblocking(fd)) {
        close(fd);
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    return vela__rt__socket_alloc_slot(fd, within);
}

static inline int64_t vela__rt__web_socket_bind(int64_t handle, vela__rt__text host, int64_t port) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);

    char host_buf[64];
    size_t host_len = (size_t)(host.len < 0 ? 0 : host.len);
    if (host_len >= sizeof(host_buf)) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    if (host_len == 0 || (host_len == 7 && memcmp(host.bytes, "0.0.0.0", 7) == 0)) {
        addr.sin_addr.s_addr = htonl(INADDR_ANY);
    } else if (host_len == 9 && memcmp(host.bytes, "127.0.0.1", 9) == 0) {
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    } else {
        memcpy(host_buf, host.bytes, host_len);
        host_buf[host_len] = '\0';
        if (inet_pton(AF_INET, host_buf, &addr.sin_addr) <= 0) {
            return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
        }
    }

    if (bind(slot->fd, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
        if (errno == EADDRINUSE) {
            return -vela__rt__sockets_done(VELA__RT__SOCK_ADDR_IN_USE);
        }
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    return vela__rt__sockets_done(VELA__RT__SOCK_OK);
}

static inline int64_t vela__rt__web_socket_listen(int64_t handle, int64_t backlog) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    int res = listen(slot->fd, backlog > 0 ? (int)backlog : 128);
    if (res != 0) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    return vela__rt__sockets_done(VELA__RT__SOCK_OK);
}

static inline int64_t vela__rt__web_socket_accept(int64_t handle, vela__rt__region* within) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    struct sockaddr_in client_addr;
    socklen_t client_len = sizeof(client_addr);
    int client_fd = accept(slot->fd, (struct sockaddr*)&client_addr, &client_len);
    if (client_fd < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return -vela__rt__sockets_done(VELA__RT__SOCK_WOULD_BLOCK);
        }
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    vela__rt__socket_set_nonblocking(client_fd);
    int yes = 1;
    setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, (const void*)&yes, sizeof(yes));
#ifdef SO_NOSIGPIPE
    setsockopt(client_fd, SOL_SOCKET, SO_NOSIGPIPE, (const void*)&yes, sizeof(yes));
#endif
    return vela__rt__socket_alloc_slot(client_fd, within);
}

static inline int64_t vela__rt__web_socket_connect(int64_t handle, vela__rt__text host, int64_t port) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);

    char host_buf[64];
    size_t host_len = (size_t)(host.len < 0 ? 0 : host.len);
    if (host_len >= sizeof(host_buf)) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    if (host_len == 0 || (host_len == 9 && memcmp(host.bytes, "127.0.0.1", 9) == 0)) {
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    } else {
        memcpy(host_buf, host.bytes, host_len);
        host_buf[host_len] = '\0';
        if (inet_pton(AF_INET, host_buf, &addr.sin_addr) <= 0) {
            return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
        }
    }

    int res = connect(slot->fd, (struct sockaddr*)&addr, sizeof(addr));
    if (res == 0) {
        return vela__rt__sockets_done(VELA__RT__SOCK_OK);
    }
    if (errno == EINPROGRESS || errno == EWOULDBLOCK) {
        return vela__rt__sockets_done(VELA__RT__SOCK_WOULD_BLOCK);
    }
    if (errno == ECONNREFUSED) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_CONN_REFUSED);
    }
    return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
}

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

static inline int64_t vela__rt__web_socket_write(int64_t handle, vela__rt__text data) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    if (data.len <= 0) {
        return vela__rt__sockets_done(VELA__RT__SOCK_OK);
    }
    ssize_t n = send(slot->fd, data.bytes, (size_t)data.len, MSG_NOSIGNAL);
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            vela__rt__sockets_done(VELA__RT__SOCK_WOULD_BLOCK);
            return 0;
        }
        if (errno == EPIPE || errno == ECONNRESET) {
            return -vela__rt__sockets_done(VELA__RT__SOCK_CONN_RESET);
        }
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    vela__rt__sockets_done(VELA__RT__SOCK_OK);
    return (int64_t)n;
}

static inline vela__rt__text vela__rt__web_socket_read(int64_t handle, int64_t max_bytes, vela__rt__region* into) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL || max_bytes <= 0) {
        vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
        return (vela__rt__text){ "", 0 };
    }
    size_t cap = (size_t)max_bytes;
    char* buf = vela__rt__region_alloc(into, cap, 1);
    ssize_t n = recv(slot->fd, buf, cap, 0);
    if (n == 0) {
        vela__rt__sockets_done(VELA__RT__SOCK_CLOSED);
        return (vela__rt__text){ "", 0 };
    }
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            vela__rt__sockets_done(VELA__RT__SOCK_WOULD_BLOCK);
            return (vela__rt__text){ "", 0 };
        }
        if (errno == ECONNRESET) {
            vela__rt__sockets_done(VELA__RT__SOCK_CONN_RESET);
            return (vela__rt__text){ "", 0 };
        }
        vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
        return (vela__rt__text){ "", 0 };
    }
    vela__rt__sockets_done(VELA__RT__SOCK_OK);
    return (vela__rt__text){ buf, (int64_t)n };
}

static inline int64_t vela__rt__web_socket_poll(int64_t handle, int64_t events, int64_t timeout_ms) {
    vela__rt__socket_slot* slot = vela__rt__socket_of(handle);
    if (slot == NULL) {
        return -vela__rt__sockets_done(VELA__RT__SOCK_INVALID_HANDLE);
    }
    struct pollfd pfd;
    pfd.fd = slot->fd;
    pfd.events = 0;
    if (events & 1) pfd.events |= POLLIN;
    if (events & 2) pfd.events |= POLLOUT;
    pfd.revents = 0;

    int res = poll(&pfd, 1, (int)timeout_ms);
    if (res < 0) {
        if (errno == EINTR) {
            return vela__rt__sockets_done(VELA__RT__SOCK_WOULD_BLOCK);
        }
        return -vela__rt__sockets_done(VELA__RT__SOCK_FAILED);
    }
    if (res == 0) {
        return vela__rt__sockets_done(VELA__RT__SOCK_TIMEOUT);
    }
    int64_t ready = 0;
    if (pfd.revents & (POLLIN | POLLHUP | POLLERR)) {
        ready |= 1;
    }
    if (pfd.revents & POLLOUT) {
        ready |= 2;
    }
    vela__rt__sockets_done(VELA__RT__SOCK_OK);
    return ready;
}

static inline int64_t vela__rt__web_time_ms(void) {
    struct timespec ts;
#if defined(CLOCK_MONOTONIC)
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0) {
        return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
    }
#endif
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
}

static inline int64_t vela__rt__web_sleep_ms(int64_t ms) {
    if (ms > 0) {
        poll(NULL, 0, (int)ms);
    }
    return 0;
}

typedef struct vela__rt__poll_entry {
    int64_t handle;
    int16_t events;
    int16_t revents;
} vela__rt__poll_entry;

typedef struct vela__rt__poll_table {
    vela__rt__poll_entry entries[256];
    size_t count;
} vela__rt__poll_table;

static inline vela__rt__poll_table* vela__rt__poll_table_get(void) {
    static vela__rt__poll_table table = { {{0, 0, 0}}, 0 };
    return &table;
}

static inline void vela__rt__web_poll_clear(void) {
    vela__rt__poll_table_get()->count = 0;
}

static inline int64_t vela__rt__web_poll_add(int64_t handle, int64_t events) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    for (size_t i = 0; i < table->count; i++) {
        if (table->entries[i].handle == handle) {
            table->entries[i].events = (int16_t)events;
            table->entries[i].revents = 0;
            return 0;
        }
    }
    if (table->count >= 256) {
        return -1;
    }
    table->entries[table->count].handle = handle;
    table->entries[table->count].events = (int16_t)events;
    table->entries[table->count].revents = 0;
    table->count++;
    return 0;
}

static inline int64_t vela__rt__web_poll_remove(int64_t handle) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    for (size_t i = 0; i < table->count; i++) {
        if (table->entries[i].handle == handle) {
            table->entries[i] = table->entries[table->count - 1];
            table->count--;
            return 0;
        }
    }
    return -1;
}

static inline int64_t vela__rt__web_poll_wait(int64_t timeout_ms) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    if (table->count == 0) {
        if (timeout_ms > 0) {
            poll(NULL, 0, (int)timeout_ms);
        }
        return 0;
    }
    struct pollfd pfds[256];
    size_t valid = 0;
    size_t map[256];
    for (size_t i = 0; i < table->count; i++) {
        table->entries[i].revents = 0;
        vela__rt__socket_slot* slot = vela__rt__socket_of(table->entries[i].handle);
        if (slot != NULL && slot->fd >= 0) {
            pfds[valid].fd = slot->fd;
            pfds[valid].events = 0;
            if (table->entries[i].events & 1) pfds[valid].events |= POLLIN;
            if (table->entries[i].events & 2) pfds[valid].events |= POLLOUT;
            pfds[valid].revents = 0;
            map[valid] = i;
            valid++;
        }
    }
    if (valid == 0) {
        if (timeout_ms > 0) {
            poll(NULL, 0, (int)timeout_ms);
        }
        return 0;
    }
    int res = poll(pfds, (nfds_t)valid, (int)timeout_ms);
    if (res <= 0) {
        return res < 0 && errno != EINTR ? -1 : 0;
    }
    int64_t ready_count = 0;
    for (size_t j = 0; j < valid; j++) {
        int64_t rev = 0;
        if (pfds[j].revents & (POLLIN | POLLHUP | POLLERR)) rev |= 1;
        if (pfds[j].revents & POLLOUT) rev |= 2;
        table->entries[map[j]].revents = (int16_t)rev;
        if (rev != 0) ready_count++;
    }
    return ready_count;
}

static inline int64_t vela__rt__web_poll_revents(int64_t handle) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    for (size_t i = 0; i < table->count; i++) {
        if (table->entries[i].handle == handle) {
            return (int64_t)table->entries[i].revents;
        }
    }
    return 0;
}

static inline bool vela__rt__web_poll_is_readable(int64_t handle) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    for (size_t i = 0; i < table->count; i++) {
        if (table->entries[i].handle == handle) {
            return (table->entries[i].revents & 1) != 0;
        }
    }
    return false;
}

static inline bool vela__rt__web_poll_is_writable(int64_t handle) {
    vela__rt__poll_table* table = vela__rt__poll_table_get();
    for (size_t i = 0; i < table->count; i++) {
        if (table->entries[i].handle == handle) {
            return (table->entries[i].revents & 2) != 0;
        }
    }
    return false;
}

static inline int64_t vela__rt__web_text_find(vela__rt__text haystack, vela__rt__text needle, int64_t from) {
    if (from < 0 || from > haystack.len || needle.len < 0) {
        return -1;
    }
    if (needle.len == 0) {
        return from;
    }
    if (haystack.len - from < needle.len) {
        return -1;
    }
    int64_t max_pos = haystack.len - needle.len;
    for (int64_t i = from; i <= max_pos; i++) {
        if (haystack.bytes[i] == needle.bytes[0] &&
            memcmp(haystack.bytes + i, needle.bytes, (size_t)needle.len) == 0) {
            return i;
        }
    }
    return -1;
}

static inline vela__rt__text vela__rt__web_text_lower(vela__rt__text s, vela__rt__region* into) {
    if (s.len <= 0) {
        return (vela__rt__text){ "", 0 };
    }
    char* buf = vela__rt__region_alloc(into, (size_t)s.len, 1);
    for (int64_t i = 0; i < s.len; i++) {
        char c = s.bytes[i];
        buf[i] = (c >= 'A' && c <= 'Z') ? (char)(c + ('a' - 'A')) : c;
    }
    return (vela__rt__text){ buf, s.len };
}

static inline vela__rt__text vela__rt__web_text_trim(vela__rt__text s) {
    int64_t start = 0;
    while (start < s.len && ((unsigned char)s.bytes[start] <= ' ')) {
        start++;
    }
    int64_t end = s.len;
    while (end > start && ((unsigned char)s.bytes[end - 1] <= ' ')) {
        end--;
    }
    return (vela__rt__text){ s.bytes + start, end - start };
}

/* Bytes from..to of s, kept within it, with no check for where characters start. */
static inline vela__rt__text vela__rt__web_text_bytes(vela__rt__text s, int64_t from, int64_t to) {
    if (to > s.len) {
        to = s.len;
    }
    if (from < 0) {
        from = 0;
    }
    if (from > to) {
        from = to;
    }
    return (vela__rt__text){ s.bytes + from, to - from };
}

static inline bool vela__rt__web_text_eq_ignore_case(vela__rt__text a, vela__rt__text b) {
    if (a.len != b.len) {
        return false;
    }
    for (int64_t i = 0; i < a.len; i++) {
        char ca = (a.bytes[i] >= 'A' && a.bytes[i] <= 'Z') ? (char)(a.bytes[i] + 32) : a.bytes[i];
        char cb = (b.bytes[i] >= 'A' && b.bytes[i] <= 'Z') ? (char)(b.bytes[i] + 32) : b.bytes[i];
        if (ca != cb) {
            return false;
        }
    }
    return true;
}

#endif

