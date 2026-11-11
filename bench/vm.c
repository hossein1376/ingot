#include <stdio.h>
#include <stdint.h>
enum { PUSH, LOAD, STORE, ADD, MUL, REM, LT, JZ, JMP, HALT };
static int64_t exec(const uint8_t *ops, const int64_t *args) {
    int64_t st[64] = {0}, regs[4] = {0}, sp = 0, pc = 0;
    for (;;) {
        uint8_t op = ops[pc]; int64_t a = args[pc]; pc++;
        switch (op) {
        case PUSH: st[sp++] = a; break;
        case LOAD: st[sp++] = regs[a]; break;
        case STORE: regs[a] = st[--sp]; break;
        case ADD: sp--; st[sp - 1] = st[sp - 1] + st[sp]; break;
        case MUL: sp--; st[sp - 1] = st[sp - 1] * st[sp]; break;
        case REM: sp--; st[sp - 1] = st[sp - 1] % st[sp]; break;
        case LT: sp--; st[sp - 1] = st[sp - 1] < st[sp]; break;
        case JZ: sp--; if (st[sp] == 0) pc = a; break;
        case JMP: pc = a; break;
        case HALT: return regs[1];
        }
    }
}
int main(void) {
    static const uint8_t ops[18] = { LOAD, PUSH, LT, JZ, LOAD, LOAD, LOAD, MUL, PUSH, REM, ADD, STORE, LOAD, PUSH, ADD, STORE, JMP, HALT };
    static const int64_t args[18] = { 0, 3000000, 0, 17, 1, 0, 0, 0, 7, 0, 0, 1, 0, 1, 0, 0, 0, 0 };
    printf("%lld\n", (long long)exec(ops, args));
    return 0;
}
