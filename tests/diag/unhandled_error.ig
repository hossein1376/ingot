enum IoErr: u8 {
    closed = 1,
}

fn read_byte(fd: i32) -> u8 ! IoErr {
    if fd < 0 {
        return @err(IoErr.closed);
    }
    return 65 as u8;
}

fn first(fd: i32) -> u8 ! IoErr {
    let b: u8 = read_byte(fd);
    return b;
}
