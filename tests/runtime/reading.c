#include "vela_rt.h"

static int failures = 0;

static vela__rt__text text(const char* s) {
    vela__rt__text t = { s, (int64_t)strlen(s) };
    return t;
}

static void whole(const char* s, int expected, int64_t value) {
    int64_t got = 0;
    int read = vela__rt__int_of_text(text(s), &got);
    if (read != expected || (read == 0 && got != value)) {
        printf("int(\"%s\"): read %d value %lld, wanted %d %lld\n", s, read, (long long)got, expected, (long long)value);
        failures++;
    }
}

static void number(const char* s, int expected, double value) {
    double got = 0;
    int read = vela__rt__float_of_text(text(s), &got);
    if (read != expected || (read == 0 && got != value)) {
        printf("float(\"%s\"): read %d value %.17g, wanted %d %.17g\n", s, read, got, expected, value);
        failures++;
    }
}

int main(void) {
    whole("42", 0, 42);
    whole("0", 0, 0);
    whole("-0", 0, 0);
    whole("007", 0, 7);
    /* Other bases, after the sign; a leading 0 alone is a digit. */
    whole("010", 0, 10);
    whole("0x1F", 0, 31);
    whole("0xaB", 0, 171);
    whole("-0b101", 0, -5);
    whole("0o17", 0, 15);
    whole("0x7fffffffffffffff", 0, INT64_MAX);
    whole("-0x8000000000000000", 0, INT64_MIN);
    whole("0x8000000000000000", 2, 0);
    whole("0x", 1, 0);
    whole("0X1", 1, 0);
    whole("0b2", 1, 0);
    whole("0o8", 1, 0);
    whole("0x1_0", 1, 0);
    whole("0x-1", 1, 0);
    whole("-7", 0, -7);
    whole("9223372036854775807", 0, INT64_MAX);
    whole("-9223372036854775808", 0, INT64_MIN);
    whole("9223372036854775808", 2, 0);
    whole("-9223372036854775809", 2, 0);
    whole("99999999999999999999999999", 2, 0);
    whole("", 1, 0);
    whole("-", 1, 0);
    whole(" 42", 1, 0);
    whole("42 ", 1, 0);
    whole("+42", 1, 0);
    whole("4x2", 1, 0);
    whole("4.2", 1, 0);
    whole("1_000", 1, 0);
    whole("1e5", 1, 0);
    whole("--1", 1, 0);
    whole("4-2", 1, 0);

    number("2.5", 0, 2.5);
    number("0.1", 0, 0.1);
    number("42", 0, 42.0);
    number("-2.5", 0, -2.5);
    number("0", 0, 0.0);
    number("0.0", 0, 0.0);
    number("000.500", 0, 0.5);
    number("123456789.123456789", 0, 123456789.123456789);
    number("0.30000000000000004", 0, 0.30000000000000004);
    number("", 1, 0);
    number(".5", 1, 0);
    number("1.", 1, 0);
    number("1.2.3", 1, 0);
    number("1e5", 1, 0);
    number("inf", 1, 0);
    number("nan", 1, 0);
    number("0x10", 1, 0);
    number(" 2.5", 1, 0);
    char big[512];
    memset(big, '9', sizeof big);
    big[500] = 0;
    number(big, 2, 0);
    char tiny[520];
    strcpy(tiny, "0.");
    memset(tiny + 2, '0', 500);
    tiny[502] = '1';
    tiny[503] = 0;
    number(tiny, 0, 0.0);
    if (failures == 0) {
        puts("ok");
    }
    return failures == 0 ? 0 : 1;
}
