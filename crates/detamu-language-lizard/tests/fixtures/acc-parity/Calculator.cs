namespace Parity;

public static class Calculator
{
    public static int Score(int value, bool enabled)
    {
        if (!enabled)
        {
            return 0;
        }

        return value > 10 ? value * 2 : value + 1;
    }
}
