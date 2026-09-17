/* The checks for arrays and slices in runtime/vela_rt.h, tested directly on
 * the paths that don't stop the program (tests/trap has programs for those):
 * indexes and slice bounds at the edges of their ranges, and room for items in
 * a region. Compiled by tests/runtime.rs as strict C11 (and with sanitizers
 * where available); prints "ok" when every check passes. */

#include "vela_rt.h"

static int failures = 0;

static void check(bool ok, const char* what) {
    if (!ok) {
        printf("failed: %s\n", what);
        failures++;
    }
}

int main(void) {
    /* An index is kept when it is one of the items. */
    check(vela__rt__index(0, 1, "1:1") == 0, "index 0 of 1 item");
    check(vela__rt__index(4, 5, "1:1") == 4, "the last index");
    check(vela__rt__index(INT64_MAX - 1, INT64_MAX, "1:1") == INT64_MAX - 1, "the largest index");

    /* Slice bounds may be equal, and may reach the end. */
    vela__rt__part(0, 0, 0, "1:1");
    vela__rt__part(0, 4, 4, "1:1");
    vela__rt__part(4, 4, 4, "1:1");
    vela__rt__part(1, 3, 4, "1:1");

    /* Room for items: none, a few (aligned), and more than a chunk holds. */
    vela__rt__region r[1] = { VELA__RT__REGION_EMPTY };
    void* none = vela__rt__region_items(r, 0, sizeof(int64_t), _Alignof(int64_t), "1:1");
    check(none != NULL, "room for no items is still a place");
    int64_t* few = vela__rt__region_items(r, 3, sizeof(int64_t), _Alignof(int64_t), "1:1");
    check(((uintptr_t)few & (_Alignof(int64_t) - 1)) == 0, "items are aligned");
    few[0] = 1;
    few[2] = 3;
    check(few[0] + few[2] == 4, "items can be written and read");
    size_t many = 100000;
    double* large = vela__rt__region_items(r, (int64_t)many, sizeof(double), _Alignof(double), "1:1");
    large[0] = 1.5;
    large[many - 1] = 2.5;
    check(large[0] + large[many - 1] == 4.0, "a large run of items is usable end to end");
    vela__rt__region_free(r);

    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
