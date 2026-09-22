/* The copies in runtime/vela_rt.h (`value.copy() at r`), tested directly: text,
 * items, pointers that share what they point to, cycles, a list longer than a C
 * stack could walk by recursion, and a table that grows many times. Compiled by
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

typedef struct Node Node;
struct Node {
    int64_t value;
    vela__rt__text label;
    Node* next;
    Node* other;
};

/* What generated code does for `Node`: copies its text and its pointers. */
static void fix_node(vela__rt__copy* c, void* at) {
    Node* node = at;
    node->label = vela__rt__copy_text(c, node->label);
    node->next = vela__rt__copy_pointer(c, node->next, sizeof(Node), _Alignof(Node), fix_node);
    node->other = vela__rt__copy_pointer(c, node->other, sizeof(Node), _Alignof(Node), fix_node);
}

static Node* make(vela__rt__region* r, int64_t value, vela__rt__text label, Node* next) {
    Node node = { value, label, next, NULL };
    return vela__rt__new(r, &node, sizeof node, _Alignof(Node));
}

int main(void) {
    vela__rt__region from[1] = { VELA__RT__REGION_EMPTY };
    vela__rt__region to[1] = { VELA__RT__REGION_EMPTY };

    /* Text: the same bytes, a NUL among them, in the other region. */
    vela__rt__copy c = { .r = to };
    vela__rt__text text = vela__rt__copy_text(&c, VELA__RT__TEXT("he\0lo"));
    check(text.len == 5 && memcmp(text.bytes, "he\0lo", 5) == 0, "text keeps its bytes");
    check(to->newest != NULL && (const char*)text.bytes >= (char*)to->newest->data, "text is in the region copied into");
    check(vela__rt__copy_text(&c, VELA__RT__TEXT("")).len == 0, "empty text");
    vela__rt__copy_finish(&c);

    /* Items: byte for byte, and no items at all. */
    c = (vela__rt__copy){ .r = to };
    int64_t numbers[3] = { 1, 2, 3 };
    int64_t* items = vela__rt__copy_items(&c, numbers, 3, sizeof(int64_t), _Alignof(int64_t));
    check(items != numbers && items[0] == 1 && items[2] == 3, "items are copied");
    check(vela__rt__copy_items(&c, numbers, 0, sizeof(int64_t), _Alignof(int64_t)) == vela__rt__no_items(), "no items copy nothing");
    check(vela__rt__copy_pointer(&c, NULL, sizeof(Node), _Alignof(Node), fix_node) == NULL, "none stays none");
    vela__rt__copy_finish(&c);

    /* Two pointers to one node give two pointers to one copy, and a cycle
     * stays a cycle. */
    Node* shared = make(from, 7, VELA__RT__TEXT("shared"), NULL);
    Node* first = make(from, 1, VELA__RT__TEXT("first"), shared);
    first->other = shared;
    shared->other = first;
    c = (vela__rt__copy){ .r = to };
    Node* copied = vela__rt__copy_pointer(&c, first, sizeof(Node), _Alignof(Node), fix_node);
    vela__rt__copy_finish(&c);
    check(copied != first && copied->value == 1, "a pointer gives a copy of its node");
    check(copied->next == copied->other && copied->next != shared, "a shared node is copied once");
    check(copied->next->other == copied, "a cycle comes back to the copy");
    check(vela__rt__text_eq(copied->next->label, VELA__RT__TEXT("shared")) && copied->next->label.bytes != shared->label.bytes, "text in nodes is copied");

    /* A long list: copied without a C stack frame per node, with the table
     * growing many times; the original's region is freed first. */
    Node* head = NULL;
    int64_t count = 300000;
    for (int64_t i = 0; i < count; i++) {
        head = make(from, i, VELA__RT__TEXT("item"), head);
    }
    c = (vela__rt__copy){ .r = to };
    Node* list = vela__rt__copy_pointer(&c, head, sizeof(Node), _Alignof(Node), fix_node);
    vela__rt__copy_finish(&c);
    vela__rt__region_free(from);
    int64_t seen = 0;
    int64_t sum = 0;
    for (Node* node = list; node != NULL; node = node->next) {
        seen++;
        sum += node->value;
    }
    check(seen == count, "every node of a long list is copied");
    check(sum == count * (count - 1) / 2, "the nodes keep their values");

    vela__rt__region_free(to);
    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
