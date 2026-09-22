/* The checks for arrays and slices in runtime/vela_rt.h, tested directly on
 * the paths that don't stop the program (tests/trap has programs for those):
 * indexes and slice bounds at the edges of their ranges, room for items in
 * a region, and slices growing and shrinking (`add` and `del`). Compiled by tests/runtime.rs as strict C11 (and with sanitizers
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
    vela__rt__part(0, 0, 0, "slice", "1:1");
    vela__rt__part(0, 4, 4, "slice", "1:1");
    vela__rt__part(4, 4, 4, "slice", "1:1");
    vela__rt__part(1, 3, 4, "slice", "1:1");

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

    /* Slices that grow: a slice with no room copies its items into a block of
     * room, which later adds fill in place. */
    typedef struct { int64_t* items; int64_t len; vela__rt__room* room; } ints;
    int64_t start[2] = { 1, 2 };
    ints xs = { start, 2, NULL };
    int64_t* moved = vela__rt__items_insert(r, xs.items, xs.len, &xs.room, 1, sizeof(int64_t), _Alignof(int64_t));
    check(moved != start && xs.room != NULL, "a slice without room moves into a block");
    moved[1] = 9;
    xs.items = moved;
    xs.len = 3;
    check(xs.items[0] == 1 && xs.items[1] == 9 && xs.items[2] == 2, "an item goes before the index, the rest after it");
    check(start[0] == 1 && start[1] == 2, "the old items are left as they were");
    int64_t* same = vela__rt__items_insert(r, xs.items, xs.len, &xs.room, 3, sizeof(int64_t), _Alignof(int64_t));
    check(same == xs.items, "a slice with room adds in place");
    same[3] = 4;
    xs.len = 4;

    /* The block is the newest memory of its chunk, so it grows in place too. */
    int64_t* grown = vela__rt__items_insert(r, xs.items, xs.len, &xs.room, 0, sizeof(int64_t), _Alignof(int64_t));
    check(grown == xs.items, "a full block at the end of its chunk grows in place");
    grown[0] = 0;
    xs.len = 5;
    check(xs.items[0] == 0 && xs.items[1] == 1 && xs.items[2] == 9 && xs.items[4] == 4, "items moved up in order");

    /* A copy made before an add sees a different end, so its own add moves it. */
    ints copy = xs;
    int64_t* added = vela__rt__items_insert(r, xs.items, xs.len, &xs.room, 5, sizeof(int64_t), _Alignof(int64_t));
    added[5] = 5;
    xs.len = 6;
    int64_t* apart = vela__rt__items_insert(r, copy.items, copy.len, &copy.room, 5, sizeof(int64_t), _Alignof(int64_t));
    apart[5] = 50;
    check(apart != xs.items && xs.items[5] == 5 && apart[5] == 50, "a copy's add doesn't write over the slice's item");
    check(copy.room != xs.room, "the copy has a room of its own");

    /* Something else allocated after the block keeps it from growing in place. */
    ints ys = { vela__rt__no_items(), 0, NULL };
    for (int64_t i = 0; i < 4; i++) {
        ys.items = vela__rt__items_insert(r, ys.items, ys.len, &ys.room, ys.len, sizeof(int64_t), _Alignof(int64_t));
        ys.items[i] = i;
        ys.len++;
    }
    (void)vela__rt__region_alloc(r, 1, 1);
    int64_t* before = ys.items;
    ys.items = vela__rt__items_insert(r, ys.items, ys.len, &ys.room, 4, sizeof(int64_t), _Alignof(int64_t));
    ys.items[4] = 4;
    ys.len++;
    check(ys.items != before && ys.items[0] == 0 && ys.items[3] == 3 && ys.items[4] == 4, "a block that can't grow moves");
    check(((uintptr_t)ys.items & (_Alignof(int64_t) - 1)) == 0, "moved items are aligned");

    /* Taking out an item closes the gap; the last item of the block gives its
     * space back, so the next add is in place. */
    vela__rt__items_remove(ys.items, ys.len, ys.room, 1, sizeof(int64_t));
    ys.len--;
    check(ys.items[0] == 0 && ys.items[1] == 2 && ys.items[3] == 4, "items after the one taken out move down");
    char* end = ys.room->end;
    vela__rt__items_remove(ys.items, ys.len, ys.room, ys.len - 1, sizeof(int64_t));
    ys.len--;
    check(ys.room->end == end - sizeof(int64_t), "the last item's space goes back to the room");
    ints part = { ys.items, 1, ys.room };
    vela__rt__items_remove(part.items, part.len, part.room, 0, sizeof(int64_t));
    check(ys.room->end == end - sizeof(int64_t), "a part ending before the room's end gives nothing back");

    /* Items aligned more strictly than the room are aligned in their block. */
    typedef struct { max_align_t value; } wide;
    wide one = { 0 };
    wide* wides = vela__rt__items_insert(r, &one, 1, &(vela__rt__room*){ NULL }, 1, sizeof(wide), _Alignof(wide));
    check(((uintptr_t)wides & (_Alignof(wide) - 1)) == 0, "strictly aligned items are aligned");

    /* The address of no items is aligned for any type. */
    check(((uintptr_t)vela__rt__no_items() & (_Alignof(max_align_t) - 1)) == 0, "no items has an aligned address");
    vela__rt__not_empty(1, "1:1");
    vela__rt__region_free(r);

    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
