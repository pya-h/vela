#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdbool.h>

typedef struct {
    int64_t key;
    int64_t val;
    bool used;
} Entry;

typedef struct {
    Entry *entries;
    size_t cap;
    size_t len;
} Map;

static uint64_t hash_i64(int64_t x) {
    uint64_t z = (uint64_t)x + 0x9e3779b97f4a7c15ULL;
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
    return z ^ (z >> 31);
}

static void map_init(Map *m, size_t cap) {
    m->cap = cap;
    m->len = 0;
    m->entries = (Entry*)calloc(cap, sizeof(Entry));
}

static void map_grow(Map *m) {
    size_t new_cap = m->cap * 2;
    Entry *new_entries = (Entry*)calloc(new_cap, sizeof(Entry));
    size_t mask = new_cap - 1;
    for (size_t i = 0; i < m->cap; i++) {
        if (m->entries[i].used) {
            size_t idx = (size_t)hash_i64(m->entries[i].key) & mask;
            while (new_entries[idx].used) {
                idx = (idx + 1) & mask;
            }
            new_entries[idx] = m->entries[i];
        }
    }
    free(m->entries);
    m->entries = new_entries;
    m->cap = new_cap;
}

static void map_inc(Map *m, int64_t key) {
    if ((m->len + 1) * 10 >= m->cap * 7) {
        map_grow(m);
    }
    size_t mask = m->cap - 1;
    size_t idx = (size_t)hash_i64(key) & mask;
    while (m->entries[idx].used) {
        if (m->entries[idx].key == key) {
            m->entries[idx].val += 1;
            return;
        }
        idx = (idx + 1) & mask;
    }
    m->entries[idx].used = true;
    m->entries[idx].key = key;
    m->entries[idx].val = 1;
    m->len++;
}

int main(void) {
    Map counts;
    map_init(&counts, 1024);
    int64_t seed = 123456789;
    for (int64_t i = 0; i < 5000000; i++) {
        seed = (seed * 1103515245 + 12345) % 2147483648LL;
        int64_t key = seed % 200000;
        map_inc(&counts, key);
    }
    int64_t total = 0;
    for (size_t i = 0; i < counts.cap; i++) {
        if (counts.entries[i].used) {
            total += counts.entries[i].val;
        }
    }
    printf("unique: %zu total: %ld\n", counts.len, (long)total);
    free(counts.entries);
    return 0;
}
