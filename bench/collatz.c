#include <stdio.h>
#include <stdint.h>
static int64_t steps(int64_t n) {
    int64_t s = 0;
    while (n != 1) { if ((n & 1) == 0) n >>= 1; else n = 3 * n + 1; s++; }
    return s;
}
int main(void) {
    int64_t total = 0;
    for (int64_t i = 1; i < 1000000; i++) total += steps(i);
    printf("%lld\n", (long long)total);
    return 0;
}
