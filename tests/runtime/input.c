/* The readers of numbers in input in runtime/vela_rt.h, tested directly:
 * `scan("Age: ", int)` and `scan("Size: ", float)`, with a line that holds a
 * number, one that holds none, the end of the input, a last line without a line
 * break, a Windows line break, and a line far longer than the reader's first
 * buffer. The prompt is empty here, so that this prints nothing but its result.
 * Compiled by tests/runtime.rs as strict C11 (and with sanitizers where
 * available); prints "ok" when every check passes. */

#include "vela_rt.h"

static int failures = 0;

/* The name of the file this test reads as its input, in the directory it runs in. */
static const char* const INPUT = "vela-input-test.txt";

static const vela__rt__text NO_PROMPT = { "", 0 };

/* Makes the input `bytes`, of `size` bytes so that a `\0` can be one of them,
 * and reads it as the standard input. */
static void input_bytes(const char* bytes, size_t size) {
    FILE* file = fopen(INPUT, "wb");
    if (file == NULL) {
        puts("cannot write the input file");
        exit(1);
    }
    if (size > 0) {
        fwrite(bytes, 1, size, file);
    }
    fclose(file);
    if (freopen(INPUT, "rb", stdin) == NULL) {
        puts("cannot read the input file");
        exit(1);
    }
}

/* The same, for input that is text as written. */
static void input(const char* bytes) {
    input_bytes(bytes, strlen(bytes));
}

static void whole(const char* what, int expected, int64_t value) {
    int64_t got = 0;
    int read = vela__rt__int_of_input(NO_PROMPT, &got);
    if (read != expected || (read == 0 && got != value)) {
        printf("%s: read %d value %lld, wanted %d %lld\n", what, read, (long long)got, expected, (long long)value);
        failures++;
    }
}

static void number(const char* what, int expected, double value) {
    double got = 0;
    int read = vela__rt__float_of_input(NO_PROMPT, &got);
    if (read != expected || (read == 0 && got != value)) {
        printf("%s: read %d value %.17g, wanted %d %.17g\n", what, read, got, expected, value);
        failures++;
    }
}

int main(void) {
    /* Line after line, then the end of the input. */
    input("42\n-7\nx\n9223372036854775808\n\n");
    whole("42", VELA__RT__READ_OK, 42);
    whole("-7", VELA__RT__READ_OK, -7);
    whole("x", VELA__RT__READ_NOT_A_NUMBER, 0);
    whole("9223372036854775808", VELA__RT__READ_TOO_LARGE, 0);
    whole("an empty line", VELA__RT__READ_NOT_A_NUMBER, 0);
    whole("the end", VELA__RT__READ_ENDED, 0);
    whole("the end again", VELA__RT__READ_ENDED, 0);

    /* Input with nothing in it has no first line. */
    input("");
    whole("nothing at all", VELA__RT__READ_ENDED, 0);

    /* A last line without a line break is still a line. */
    input("7");
    whole("no line break", VELA__RT__READ_OK, 7);
    whole("after it", VELA__RT__READ_ENDED, 0);

    /* A Windows line break is not part of the number. */
    input("2.5\r\n-0.5\r\n");
    number("2.5 with a carriage return", VELA__RT__READ_OK, 2.5);
    number("-0.5 with a carriage return", VELA__RT__READ_OK, -0.5);
    number("the end", VELA__RT__READ_ENDED, 0);

    /* A line far longer than the 128 bytes the reader starts with, which it
     * grows to hold, and which is a number no int holds. */
    {
        char big[5002];
        memset(big, '9', 5000);
        big[5000] = '\n';
        big[5001] = 0;
        input(big);
        whole("five thousand digits", VELA__RT__READ_TOO_LARGE, 0);
    }

    /* A `\0` is a byte of the line like any other, and no number holds it. */
    input_bytes("1\0" "2\n", 4);
    whole("a line holding a zero byte", VELA__RT__READ_NOT_A_NUMBER, 0);

    remove(INPUT);
    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
