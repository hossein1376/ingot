// '_' names are private to this module; public ones use them freely.
struct _State { n: i64 }

fn _bump(s: *_State) {
    s.*.n += 1;
}

fn count_to(k: i64) -> i64 {
    var s: _State = _State.{ n: 0 };
    for i in 0..k {
        _bump(&s);
    }
    return s.n;
}
