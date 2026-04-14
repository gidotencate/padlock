/* Struct with obvious padding waste — used by integration tests. */
struct Padded {
    char   a;      /* 1B, then 7B padding before b */
    double b;      /* 8B */
    char   c;      /* 1B, then 7B padding at end */
};

/* Struct with false sharing potential */
struct Shared {
    int lock_a;
    long counter_a;
    int lock_b;
    long counter_b;
};
