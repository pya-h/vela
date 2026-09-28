/* The writer in runtime/vela_rt.h, tested directly: what `show` and `str`
 * write for values inside others (numbers, bools, quoted text, addresses),
 * text built past the writer's first buffer, and the stack views a join reads
 * numbers from. Compiled by tests/runtime.rs as strict C11 (and with sanitizers
 * where available); prints "ok" when every check passes. */

#include "vela_rt.h"

static int failures = 0;

static void same(const char* what, vela__rt__text got, const char* wanted) {
    size_t len = strlen(wanted);
    if ((size_t)got.len != len || memcmp(got.bytes, wanted, len) != 0) {
        printf("%s: got [%.*s], wanted [%s]\n", what, (int)got.len, got.bytes, wanted);
        failures++;
    }
}

int main(void) {
    vela__rt__region r[1] = { VELA__RT__REGION_EMPTY };

    /* Numbers, bools, and text as values inside others are written. */
    vela__rt__writer w = VELA__RT__BUILDER;
    vela__rt__put_int(&w, -42);
    VELA__RT__PUT(&w, " ");
    vela__rt__put_float(&w, 2.5);
    VELA__RT__PUT(&w, " ");
    vela__rt__put_float(&w, 0.0 / 0.0);
    VELA__RT__PUT(&w, " ");
    vela__rt__put_bool(&w, true);
    VELA__RT__PUT(&w, " ");
    vela__rt__put_quoted(&w, VELA__RT__TEXT("a \"b\"\\\n\t\r"));
    VELA__RT__PUT(&w, " ");
    vela__rt__put_text(&w, VELA__RT__TEXT("bare"));
    same("values", vela__rt__built(&w, r), "-42 2.5 nan true \"a \\\"b\\\"\\\\\\n\\t\\r\" bare");

    /* A zero inside text is escaped, and the rest of the text kept. */
    vela__rt__put_quoted(&w, (vela__rt__text){ "a\0b", 3 });
    same("zero", vela__rt__built(&w, r), "\"a\\0b\"");

    /* Nothing written is empty text. */
    same("empty", vela__rt__built(&w, r), "");

    /* Past the first buffer, the text is whole. */
    for (int i = 0; i < 1000; i++) {
        vela__rt__put_int(&w, i % 10);
    }
    vela__rt__text long_text = vela__rt__built(&w, r);
    if (long_text.len != 1000 || long_text.bytes[0] != '0' || long_text.bytes[999] != '9') {
        printf("long: %lld bytes\n", (long long)long_text.len);
        failures++;
    }

    /* An address is C's hex, which the reader reads back. */
    int64_t place = 0;
    vela__rt__put_address(&w, &place);
    vela__rt__text address = vela__rt__built(&w, r);
    int64_t read = 0;
    if (address.len < 3 || memcmp(address.bytes, "0x", 2) != 0 || vela__rt__int_of_text(address, &read) != 0 ||
        (uintptr_t)read != (uintptr_t)&place) {
        printf("address: [%.*s]\n", (int)address.len, address.bytes);
        failures++;
    }

    /* A join reads numbers from views on the stack. */
    vela__rt__text parts[] = {
        VELA__RT__TEXT("n = "),
        vela__rt__int_view((char[VELA__RT__NUMBER_ROOM]){ 0 }, INT64_MIN),
        VELA__RT__TEXT(", x = "),
        vela__rt__float_view((char[VELA__RT__NUMBER_ROOM]){ 0 }, -1.5e300),
    };
    same("join", vela__rt__join(r, parts, 4), "n = -9223372036854775808, x = -1.5e+300");

    vela__rt__region_free(r);
    if (failures == 0) {
        printf("ok\n");
    }
    return failures == 0 ? 0 : 1;
}
