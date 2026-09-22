/* The integer checks in runtime/vela_rt.h, tested directly. The runtime uses
 * the C compiler's overflow builtins where it has them; this file defines
 * VELA__RT__PORTABLE_ARITHMETIC to test the plain comparisons used elsewhere,
 * against the builtins as the reference, on every pair of values near the
 * edges. Also the results of division and remainder that don't trap. Compiled
 * by tests/runtime.rs as strict C11 (and with sanitizers where available, which
 * would catch an overflow inside a check); prints "ok" when every check passes. */

#define VELA__RT__PORTABLE_ARITHMETIC
#include "vela_rt.h"

static int failures = 0;

static void check(bool ok, const char* what, int64_t a, int64_t b) {
    if (!ok) {
        printf("failed: %s with %lld and %lld\n", what, (long long)a, (long long)b);
        failures++;
    }
}

/* Values near every edge of multiplication and addition. */
static const int64_t edges[] = {
    INT64_MIN, INT64_MIN + 1, INT64_MIN + 2, INT64_MIN / 2 - 1, INT64_MIN / 2, INT64_MIN / 2 + 1,
    -4294967297, -4294967296, -4294967295, -3037000500, -3037000499, -65536, -1000, -3, -2, -1,
    0, 1, 2, 3, 1000, 65536, 3037000499, 3037000500, 4294967295, 4294967296, 4294967297,
    INT64_MAX / 2 - 1, INT64_MAX / 2, INT64_MAX / 2 + 1, INT64_MAX - 2, INT64_MAX - 1, INT64_MAX,
};

int main(void) {
    size_t count = sizeof edges / sizeof edges[0];
#if defined(__GNUC__)
    for (size_t i = 0; i < count; i++) {
        for (size_t j = 0; j < count; j++) {
            int64_t a = edges[i], b = edges[j];
            int64_t expected, actual;
            bool overflows = __builtin_add_overflow(a, b, &expected);
            check(vela__rt__add_overflows(a, b, &actual) == overflows && (overflows || actual == expected), "add", a, b);
            overflows = __builtin_sub_overflow(a, b, &expected);
            check(vela__rt__sub_overflows(a, b, &actual) == overflows && (overflows || actual == expected), "sub", a, b);
            overflows = __builtin_mul_overflow(a, b, &expected);
            check(vela__rt__mul_overflows(a, b, &actual) == overflows && (overflows || actual == expected), "mul", a, b);
        }
    }
#endif
    /* Operations that fit give their ordinary results. */
    check(vela__rt__add(INT64_MAX - 1, 1, "") == INT64_MAX, "add", INT64_MAX - 1, 1);
    check(vela__rt__sub(INT64_MIN + 1, 1, "") == INT64_MIN, "sub", INT64_MIN + 1, 1);
    check(vela__rt__mul(-3037000499, 3037000499, "") == -9223372030926249001, "mul", -3037000499, 3037000499);
    check(vela__rt__div(-7, 2, "") == -3, "div", -7, 2);
    check(vela__rt__div(INT64_MIN, 1, "") == INT64_MIN, "div", INT64_MIN, 1);
    check(vela__rt__div(INT64_MAX, -1, "") == -INT64_MAX, "div", INT64_MAX, -1);
    check(vela__rt__mod(-7, 3, "") == -1, "mod", -7, 3);
    check(vela__rt__mod(7, -3, "") == 1, "mod", 7, -3);
    check(vela__rt__mod(INT64_MIN, -1, "") == 0, "mod", INT64_MIN, -1);
    check(vela__rt__mod(INT64_MIN, INT64_MAX, "") == -1, "mod", INT64_MIN, INT64_MAX);
    check(vela__rt__neg(INT64_MIN + 1, "") == INT64_MAX, "neg", INT64_MIN + 1, 0);
    /* `int(x)` on a float: the whole part, toward zero, which rounds neither
     * way, and the two edges a double holds exactly: -2^63, which is an int,
     * and 2^63 - 1024, the largest double below 2^63. The floats that stop the
     * program instead are in tests/trap. */
    check(vela__rt__int_of_float(2.9, "") == 2, "int of float", 2, 0);
    check(vela__rt__int_of_float(2.0, "") == 2, "int of float", 2, 0);
    check(vela__rt__int_of_float(-2.9, "") == -2, "int of float", -2, 0);
    check(vela__rt__int_of_float(0.5, "") == 0, "int of float", 0, 0);
    check(vela__rt__int_of_float(-0.5, "") == 0, "int of float", 0, 0);
    check(vela__rt__int_of_float(-0.0, "") == 0, "int of float", 0, 0);
    check(vela__rt__int_of_float((double)INT64_MIN, "") == INT64_MIN, "int of float", INT64_MIN, 0);
    check(vela__rt__int_of_float(9223372036854774784.0, "") == 9223372036854774784, "int of float", INT64_MAX, 0);
    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
