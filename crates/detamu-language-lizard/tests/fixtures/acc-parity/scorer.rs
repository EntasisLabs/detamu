pub fn score(value: u32, enabled: bool) -> u32 {
    if !enabled {
        return 0;
    }
    if value > 10 {
        value * 2
    } else {
        value + 1
    }
}
