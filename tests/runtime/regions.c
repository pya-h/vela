/* The region allocator in runtime/vela_rt.h, tested directly: alignment,
 * growth, allocations too large for a chunk, and freeing. Compiled by
 * tests/runtime.rs as strict C11 (and with sanitizers where available); prints
 * "ok" when every check passes. */

#include "vela_rt.h"

static int failures = 0;

static void check(bool ok, const char* what) {
    if (!ok) {
        printf("failed: %s\n", what);
        failures++;
    }
}

static bool aligned(void* p, size_t align) {
    return ((uintptr_t)p & (align - 1)) == 0;
}

static size_t chunks(vela__rt__region* r) {
    size_t count = 0;
    for (vela__rt__chunk* c = r->newest; c != NULL; c = c->previous) {
        count++;
    }
    return count;
}

int main(void) {
    /* An empty region costs nothing and frees nothing. */
    vela__rt__region empty[1] = { VELA__RT__REGION_EMPTY };
    check(chunks(empty) == 0, "an empty region has no chunks");
    vela__rt__region_free(empty);
    check(empty->newest == NULL, "freeing an empty region leaves it empty");

    vela__rt__region r[1] = { VELA__RT__REGION_EMPTY };

    /* Every allocation is aligned for its type, whatever came before it. */
    char c = 'v';
    char* pc = vela__rt__new(r, &c, sizeof c, _Alignof(char));
    int64_t n = 42;
    int64_t* pn = vela__rt__new(r, &n, sizeof n, _Alignof(int64_t));
    double d = 2.5;
    double* pd = vela__rt__new(r, &d, sizeof d, _Alignof(double));
    long double ld = 1.25L;
    long double* pld = vela__rt__new(r, &ld, sizeof ld, _Alignof(long double));
    check(aligned(pn, _Alignof(int64_t)) && aligned(pd, _Alignof(double)), "int64_t and double are aligned");
    check(aligned(pld, _Alignof(long double)), "long double is aligned");
    check(*pc == 'v' && *pn == 42 && *pd == 2.5 && *pld == 1.25L, "values are copied in");
    check(chunks(r) == 1, "small allocations share the first chunk");

    /* Many allocations fill chunks, each new one larger, and earlier values
     * stay intact. */
    int64_t* first = NULL;
    int64_t* last = NULL;
    for (int64_t i = 0; i < 1000000; i++) {
        int64_t* p = vela__rt__new(r, &i, sizeof i, _Alignof(int64_t));
        if (i == 0) {
            first = p;
        }
        if (last != NULL && p == last) {
            check(false, "allocations are distinct");
        }
        last = p;
    }
    check(first != NULL && *first == 0 && last != NULL && *last == 999999, "values survive growth");
    check(chunks(r) > 1 && chunks(r) < 64, "chunks grow instead of staying small");
    for (vela__rt__chunk* chunk = r->newest; chunk != NULL; chunk = chunk->previous) {
        check(chunk->size + sizeof(vela__rt__chunk) <= VELA__RT__CHUNK_MAX, "no ordinary chunk passes the largest size");
        check(chunk->used <= chunk->size, "no chunk is used past its size");
    }

    /* An allocation too large for the next chunk gets a chunk of its own,
     * behind the newest, which keeps taking small allocations. */
    vela__rt__chunk* newest = r->newest;
    size_t big = VELA__RT__CHUNK_MAX * 3;
    unsigned char* block = vela__rt__region_alloc(r, big, 1);
    memset(block, 7, big);
    check(r->newest == newest, "a large allocation leaves the newest chunk in place");
    check(newest->previous != NULL && newest->previous->size == big, "a large allocation has a chunk of exactly its size");
    check(block[0] == 7 && block[big - 1] == 7, "a large allocation is usable");

    /* Many allocations too large to share a chunk waste no space: each chunk is
     * exactly as large as its allocation. */
    vela__rt__region wide[1] = { VELA__RT__REGION_EMPTY };
    vela__rt__region_alloc(wide, 8, 8);
    size_t requested = 8;
    for (int i = 0; i < 1000; i++) {
        vela__rt__region_alloc(wide, 4100, 1);
        requested += 4100;
    }
    size_t capacity = 0;
    for (vela__rt__chunk* chunk = wide->newest; chunk != NULL; chunk = chunk->previous) {
        capacity += chunk->size;
    }
    check(capacity < requested + VELA__RT__CHUNK_FIRST, "large allocations waste no chunk space");
    vela__rt__region_free(wide);

    /* A first allocation larger than any ordinary chunk. */
    vela__rt__region huge[1] = { VELA__RT__REGION_EMPTY };
    unsigned char* first_huge = vela__rt__region_alloc(huge, (size_t)VELA__RT__CHUNK_MAX * 2, 1);
    first_huge[0] = 1;
    int64_t after = 3;
    int64_t* p_after = vela__rt__new(huge, &after, sizeof after, _Alignof(int64_t));
    check(*p_after == 3 && first_huge[0] == 1 && chunks(huge) == 2, "a small allocation after a huge first one gets a new chunk");
    vela__rt__region_free(huge);

    /* A first allocation that fits the first chunk goes there, however large a
     * part of it, and small ones after it share the chunk. */
    vela__rt__region medium[1] = { VELA__RT__REGION_EMPTY };
    vela__rt__region_alloc(medium, 2100, 1);
    vela__rt__region_alloc(medium, 8, 8);
    check(chunks(medium) == 1, "a first allocation of half a chunk or more shares its chunk");
    vela__rt__region_free(medium);

    /* After a first allocation too large for any chunk, ordinary chunks start
     * at the first size again. */
    vela__rt__region large[1] = { VELA__RT__REGION_EMPTY };
    vela__rt__region_alloc(large, VELA__RT__CHUNK_FIRST * 3 + 5, 1);
    vela__rt__region_alloc(large, 8, 8);
    check(chunks(large) == 2 && large->newest->size + sizeof(vela__rt__chunk) == VELA__RT__CHUNK_FIRST,
          "ordinary chunks after an odd-sized first chunk start at the first size");
    vela__rt__region_free(large);

    /* Freeing empties the region, which can then be used again. */
    vela__rt__region_free(r);
    check(r->newest == NULL, "freeing a region empties it");
    int64_t again = 5;
    int64_t* pa = vela__rt__new(r, &again, sizeof again, _Alignof(int64_t));
    check(*pa == 5 && chunks(r) == 1, "a freed region can allocate again");
    vela__rt__region_free(r);

    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
