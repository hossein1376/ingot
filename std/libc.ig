// std/libc.ig: commonly used C library declarations.
// Sizes use explicit Ingot types: size_t = u64, int = i32, pointers are *u8.
extern fn printf(fmt: *u8, ...) -> i32;
extern fn snprintf(buf: *u8, n: u64, fmt: *u8, ...) -> i32;
extern fn puts(s: *u8) -> i32;
extern fn write(fd: i32, buf: *u8, n: u64) -> i64;
extern fn malloc(n: u64) -> *u8;
extern fn calloc(count: u64, size: u64) -> *u8;
extern fn realloc(p: *u8, n: u64) -> *u8;
extern fn free(p: *u8);
extern fn memcpy(dst: *u8, src: *u8, n: u64) -> *u8;
extern fn memset(dst: *u8, c: i32, n: u64) -> *u8;
extern fn strlen(s: *u8) -> u64;
extern fn exit(code: i32);
extern fn sysconf(name: i32) -> i64;
