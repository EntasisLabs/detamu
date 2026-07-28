export function score(value: number, enabled: boolean): number {
    if (!enabled) {
        return 0;
    }
    return value > 10 ? value * 2 : value + 1;
}
