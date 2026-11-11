// std/io.ig: whole-file I/O through C stdio, with errors as values.
// read_file allocates with malloc; release the buffer with free(data.ptr).
import "std/libc.ig";

extern fn fopen(path: *u8, mode: *u8) -> *u8;
extern fn fclose(f: *u8) -> i32;
extern fn fread(p: *u8, size: u64, n: u64, f: *u8) -> u64;
extern fn fwrite(p: *u8, size: u64, n: u64, f: *u8) -> u64;
extern fn fseek(f: *u8, off: i64, whence: i32) -> i32;
extern fn ftell(f: *u8) -> i64;

enum IoErr: u8 {
    open_failed = 1,
    read_failed = 2,
    write_failed = 3,
    out_of_memory = 4,
}

// Reads the whole file: out.* = its bytes. One extra NUL byte is stored
// after the end, so out.*.ptr can also be passed to C as a string.
fn read_file(path: *u8, out: *[]u8) -> void ! IoErr {
    let f: *u8 = fopen(path, c"rb");
    if f == null {
        return @err(IoErr.open_failed);
    }
    defer { fclose(f); }
    if fseek(f, 0, 2) != 0 { // SEEK_END
        return @err(IoErr.read_failed);
    }
    let n: i64 = ftell(f);
    if n < 0 || fseek(f, 0, 0) != 0 { // SEEK_SET
        return @err(IoErr.read_failed);
    }
    let p: *u8 = malloc((n + 1) as u64);
    if p == null {
        return @err(IoErr.out_of_memory);
    }
    errdefer { free(p); } // the caller owns p only on success
    if fread(p, 1 as u64, n as u64, f) != n as u64 {
        return @err(IoErr.read_failed);
    }
    p[n] = 0 as u8;
    out.* = p[0..n];
}

// Creates or truncates path and writes data to it.
fn write_file(path: *u8, data: []u8) -> void ! IoErr {
    let f: *u8 = fopen(path, c"wb");
    if f == null {
        return @err(IoErr.open_failed);
    }
    let ok: bool = fwrite(data.ptr, 1 as u64, data.len as u64, f) == data.len as u64;
    if fclose(f) != 0 || !ok {
        return @err(IoErr.write_failed);
    }
}

fn roundtrip(path: *u8) -> i64 ! IoErr {
    var buf: [64]u8 = @zero;
    for i in 0..64 {
        buf[i] = (i * 3) as u8;
    }
    try write_file(path, buf[0..64]);
    var back: []u8 = buf[0..0];
    try read_file(path, &back);
    defer { free(back.ptr); }
    var sum: i64 = 0;
    for i in 0..back.len {
        sum += back[i] as i64;
    }
    @assert(back.ptr[back.len] == 0 as u8);
    return sum;
}

test "write_file then read_file" {
    let s: i64 = roundtrip(c"/tmp/ingot_io_test.bin") catch e => -1;
    @assert(s == 6048); // 3 * (0 + ... + 63)
    var nope: []u8 = @zero;
    var failed: bool = false;
    read_file(c"/nonexistent/dir/x", &nope) catch e => {
        @assert(e == IoErr.open_failed);
        failed = true;
    };
    @assert(failed);
}
