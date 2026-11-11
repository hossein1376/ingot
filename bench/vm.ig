// Interpreter kernel: a stack-machine bytecode loop. The dense switch on
// Op compiles to a jump table; the stack and registers are frame arrays.
extern fn printf(fmt: *u8, ...) -> i32;

enum Op: u8 {
    push,
    load,
    store,
    add,
    mul,
    rem,
    lt,
    jz,
    jmp,
    halt,
}

fn exec(ops: []Op, args: []i64) -> i64 {
    var st: [64]i64 = @zero;
    var regs: [4]i64 = @zero;
    var sp: i64 = 0;
    var pc: i64 = 0;
    while true {
        let op: Op = ops[pc];
        let a: i64 = args[pc];
        pc += 1;
        switch op {
            Op.push => {
                st[sp] = a;
                sp += 1;
            }
            Op.load => {
                st[sp] = regs[a];
                sp += 1;
            }
            Op.store => {
                sp -= 1;
                regs[a] = st[sp];
            }
            Op.add => {
                sp -= 1;
                st[sp - 1] = st[sp - 1] + st[sp];
            }
            Op.mul => {
                sp -= 1;
                st[sp - 1] = st[sp - 1] * st[sp];
            }
            Op.rem => {
                sp -= 1;
                st[sp - 1] = st[sp - 1] % st[sp];
            }
            Op.lt => {
                sp -= 1;
                st[sp - 1] = @select(st[sp - 1] < st[sp], 1, 0);
            }
            Op.jz => {
                sp -= 1;
                if st[sp] == 0 {
                    pc = a;
                }
            }
            Op.jmp => {
                pc = a;
            }
            Op.halt => {
                return regs[1];
            }
        }
    }
    return 0;
}

pub fn main() -> i32 {
    // r1 = sum over i < 3000000 of (i * i) % 7
    var ops: [18]Op = [18]Op.{ Op.load, Op.push, Op.lt, Op.jz, Op.load, Op.load, Op.load, Op.mul, Op.push, Op.rem, Op.add, Op.store, Op.load, Op.push, Op.add, Op.store, Op.jmp, Op.halt };
    var args: [18]i64 = [18]i64.{ 0, 3000000, 0, 17, 1, 0, 0, 0, 7, 0, 0, 1, 0, 1, 0, 0, 0, 0 };
    printf(c"%lld\n", exec(ops[0..18], args[0..18]));
    return 0;
}
