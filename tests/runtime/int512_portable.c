/* The 512-bit division and text conversion in runtime/vela_rt.h, tested
 * directly without 128-bit integers: this file defines
 * VELA__RT__PORTABLE_ARITHMETIC, which leaves every 64-bit limb step to the
 * fallback paths, and compares them against per-limb reference calculations
 * that use unsigned __int128 where the C compiler has it. The fallback's
 * single-limb division used to lose the high bits of its running remainder
 * when the divisor was large, so the divisors here are all well past 2**32.
 * Compiled by tests/runtime.rs as strict C11 (and with sanitizers where
 * available, which would catch a shift past a limb); prints "ok" when every
 * check passes. */

#define VELA__RT__PORTABLE_ARITHMETIC
#include "vela_rt.h"

static int failures = 0;

static void check(bool ok, const char* what) {
    if (!ok) {
        printf("failed: %s\n", what);
        failures++;
    }
}

static vela__rt__int512 from_u64(uint64_t v) {
    vela__rt__int512 r = {{0}};
    r.limbs[0] = v;
    return r;
}

/* A per-limb unsigned quotient and remainder, using 128-bit arithmetic as the
 * reference when the C compiler has it (the test machine does; a machine
 * without it skips the reference comparison, since the builtin path is what
 * ships there and this file exists to check the fallback against it). */
#if defined(__SIZEOF_INT128__)
static uint64_t ref_div_mod(uint64_t rem, uint64_t limb, uint64_t div, uint64_t* out_q) {
    vela__rt__u128 cur = ((vela__rt__u128)rem << 64) | limb;
    *out_q = (uint64_t)(cur / div);
    return (uint64_t)(cur % div);
}
#endif

int main(void) {
    /* Divisors a 32-bit shift would break: the smallest and largest values
     * past 2**32, and 10**19 as `int512_to_str` chunks by it. */
    const uint64_t divisors[] = {
        4294967297ULL, 9999999999999999999ULL, 18446744073709551557ULL,
    };
    for (size_t d = 0; d < sizeof divisors / sizeof divisors[0]; d++) {
        uint64_t div = divisors[d];
        vela__rt__int512 v = from_u64(div);
        /* Dividends with bits set in every limb region, built limb by limb. */
        for (int round = 0; round < 64; round++) {
            vela__rt__int512 u = {{0}};
            uint64_t state = 0x9E3779B97F4A7C15ULL ^ (div + (uint64_t)round * 0x2545F4914F6CDD1DULL);
            for (int i = 0; i < 8; i++) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                u.limbs[i] = state;
            }
            if (vela__rt__u512_cmp(u, v) < 0) {
                u.limbs[7] |= 0x8000000000000000ULL;
            }
            vela__rt__int512 q;
            vela__rt__int512 r;
            vela__rt__u512_divrem(u, v, &q, &r);
#if defined(__SIZEOF_INT128__)
            uint64_t rem = 0;
            for (int i = 7; i >= 0; i--) {
                uint64_t digit;
                rem = ref_div_mod(rem, u.limbs[i], div, &digit);
                check(q.limbs[i] == digit, "single-limb quotient digit");
            }
            check(r.limbs[0] == rem, "single-limb remainder");
            for (int i = 1; i < 8; i++) check(r.limbs[i] == 0, "single-limb remainder high limbs");
            /* The full-radix check: quotient times divisor plus remainder is
             * the dividend again. */
            vela__rt__int512 back;
            vela__rt__u512_mul(q, v, &back);
            uint64_t carry = 0;
            for (int i = 0; i < 8; i++) {
                vela__rt__u128 sum = (vela__rt__u128)back.limbs[i] + r.limbs[i] + carry;
                back.limbs[i] = (uint64_t)sum;
                carry = (uint64_t)(sum >> 64);
            }
            check(vela__rt__u512_cmp(back, u) == 0, "quotient times divisor plus remainder");
#endif
        }
    }
    /* Text conversion of a value that forces every 10**19 chunk through the
     * same fallback division. */
    vela__rt__int512 n = {{0}};
    n.limbs[7] = 0x0123456789ABCDEFULL;
    n.limbs[5] = 0xFEDCBA9876543210ULL;
    n.limbs[0] = 12345678901234567890ULL;
    /* The digits, checked against the value worked out with a big-integer
     * calculator: every chunk division goes through the fallback path. */
    const char* expected =
        "59590257466411541886248410857620380061859455589225173075914107969266735803958181"
        "858447905556853621782163274476906642637297499255148941413703997766699730";
    char buf[200];
    size_t len = vela__rt__int512_to_str(n, buf);
    check(len == strlen(expected) && strcmp(expected, buf) == 0, "to_str digits");
    if (failures > 0) {
        printf("%d failures\n", failures);
        return 1;
    }
    printf("ok\n");
    return 0;
}
