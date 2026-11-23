#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

typedef struct Node {
    int64_t val;
    struct Node *left;
    struct Node *right;
} Node;

static Node* build(int depth, int64_t seed) {
    Node *n = (Node*)malloc(sizeof(Node));
    if (depth == 0) {
        n->val = seed % 10;
        n->left = NULL;
        n->right = NULL;
        return n;
    }
    n->val = 0;
    n->left = build(depth - 1, seed * 3 + 1);
    n->right = build(depth - 1, seed * 7 + 2);
    return n;
}

static int64_t total(Node *n) {
    if (n->left == NULL && n->right == NULL) return n->val;
    return total(n->left) + total(n->right);
}

static void free_tree(Node *n) {
    if (n == NULL) return;
    free_tree(n->left);
    free_tree(n->right);
    free(n);
}

int main(void) {
    int max_depth = 16;
    int stretch_depth = max_depth + 1;

    Node *stretch_tree = build(stretch_depth, 0);
    printf("stretch tree of depth %d check: %ld\n", stretch_depth, (long)total(stretch_tree));
    free_tree(stretch_tree);

    Node *long_tree = build(max_depth, 0);

    for (int depth = 4; depth <= max_depth; depth += 2) {
        int shift = max_depth - depth + 4;
        int64_t iterations = 1LL << shift;
        int64_t sum = 0;
        for (int64_t i = 0; i < iterations; i++) {
            Node *t = build(depth, i);
            sum += total(t);
            free_tree(t);
        }
        printf("%ld trees of depth %d check: %ld\n", (long)iterations, depth, (long)sum);
    }

    printf("long lived tree of depth %d check: %ld\n", max_depth, (long)total(long_tree));
    free_tree(long_tree);
    return 0;
}
