// Hello world: Ingot calls libc directly; c"..." is a NUL-terminated *u8.
extern fn puts(s: *u8) -> i32;

pub fn main() -> i32 {
    puts(c"Hello from Ingot on AArch64!");
    return 0;
}
