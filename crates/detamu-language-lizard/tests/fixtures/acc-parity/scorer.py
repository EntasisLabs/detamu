def score(value, enabled):
    if not enabled:
        return 0
    if value > 10:
        return value * 2
    return value + 1
