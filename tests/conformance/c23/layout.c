// C23 constructs, compiled with `zig cc -std=c23` (gcc < 14 lacks _BitInt).
#include <stddef.h>
#include <stdio.h>
struct Bi { char a; _BitInt(7) b; unsigned _BitInt(33) c; _BitInt(128) d; char e; };
struct Bi2 { _BitInt(9) a; char b; signed _BitInt(65) c; unsigned _BitInt(17) d; };
struct Bi3 { char a; _BitInt(200) big; char z; };
#define P(T,L) printf(#T " %zu %zu " #L "\n", sizeof(struct T), offsetof(struct T, L))
int main(void) {
    P(Bi, e); P(Bi2, d); P(Bi3, z);
}
