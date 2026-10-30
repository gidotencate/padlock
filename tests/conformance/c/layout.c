#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <wchar.h>
#include <stdio.h>
#define N 8
enum E { A, B };
enum { K = 5 };
typedef struct Inner { char x; int y; } Inner;
struct Arr { char a; char b[48]; long c; };
struct ArrMac { char a; char b[N]; long c; };
struct ArrEnum { char a; char b[K]; long c; };
struct ArrExpr { char a; char b[N*2+1]; long c; };
struct ArrSizeof { char a; char b[sizeof(long)*3]; char c; };
struct Arr2D { char a; short b[2][3]; char c; };
struct ArrStruct { char a; Inner b[2]; char c; };
struct Enum { char a; enum E e; char c; };
struct LD { char a; long double l; char c; };
struct I128 { char a; __int128 l; char c; };
struct Cx { char a; double _Complex z; char c; };
struct CxF { char a; float _Complex z; char c; };
struct Wc { char a; wchar_t w; char c; };
struct Bool { char a; bool b; int c; };
struct Sz { char a; size_t s; ptrdiff_t p; intptr_t q; char c; };
struct LL { char a; long long b; unsigned long long c; char d; };
struct FP { char a; void (*f)(int); int (*g[2])(void); char c; };
struct Pk { char a; int b; short c; } __attribute__((packed));
struct PkOuter { char a; struct Pk p; char c; };
struct Al { char a; int b __attribute__((aligned(16))); char c; };
struct AlS { char a; char b; } __attribute__((aligned(32)));
struct AlSO { char a; struct AlS s; char c; };
struct NestedPtr { char a; struct NestedPtr *next; char c; };
struct NestInl { char a; struct { char x; long y; } n; char c; };
struct NestInlArr { char a; struct { char x; long y; } n[3]; char c; };
union U { char a; long b; char c[13]; };
struct WithU { char a; union U u; char c; };
struct WithUInl { char a; union { char x; double y; } u; char c; };
struct Flex { int n; char data[]; };
struct Flex2 { char c; long n; char data[]; };
struct Bits { unsigned a:3; unsigned b:5; char c; };
struct BitsMix { char a; int b:4; short c:4; char d; };
struct BitsL { long a:40; char b; int c:3; char t; };
struct Const { const char a; volatile int b; const char *restrict p; char c; };
struct Unsigned { unsigned char a; unsigned long b; unsigned short c; signed char d; };
struct Ptrs { char a; char **pp; const int *c; void *v; char b; };
struct Atom { char a; _Atomic int x; _Atomic(long) y; char c; };
struct Align { char a; _Alignas(8) char b; char c; };
struct F16 { char a; float f; double d; char c; };
struct Typedef { char a; Inner i; char c; };
typedef uint8_t u8; typedef uint16_t u16; typedef u16 mylen;
struct TD { u8 a; mylen b; u8 c; uint64_t d; u8 e; };
struct Arr3 { u8 a; u8 b[3]; u16 c[2]; u8 d; };
struct BfDiff { uint8_t a:3; uint32_t b:5; uint8_t t; };
struct BfAnon { int :32; int a:4; char t; };
struct BfZero { int a:3; int :0; int b:5; char t; };
struct BfWide { char a; int b:30; char c; };
struct BfPk { char a; int b:4; int c:6; char d; } __attribute__((packed));
#define HAVE_WIDE 1
#define LEVEL 3
struct Pp {
    char a;
#ifdef HAVE_WIDE
    long wide;
#else
    char narrow;
#endif
#if LEVEL > 2 && defined(HAVE_WIDE)
    int lvl;
#elif LEVEL > 1
    short lvl2;
#endif
#ifndef NOT_DEFINED_ANYWHERE
    short s;
#endif
#if 0
    char dead[100];
#endif
    char c;
};
#if 0
struct Dup { char a; long b; char c; };
#else
struct Dup { char a; char c; };
#endif
struct ZeroArr { char a; int z[0]; char c; };
/* A forward struct typedef, macro-built members and macro-built types. */
typedef struct FwdT FwdT_t;
struct FwdT { char a; long b; };
struct UsesFwd { char a; FwdT_t f; char c; };
#define MEMBERS(p) int p##x; char p##y
struct MacMem { char a; MEMBERS(m_); long c; };
typedef float V4 __attribute__((__vector_size__(16)));
struct Vec { char a; V4 v; char c; };
#define WORD_T long int
#define MY_T WORD_T
typedef MY_T word_t;
struct MacTy { char a; word_t w; char c; };
/* Union member alignment and #pragma pack with nested aggregates (found by fuzzing). */
union UAlC { size_t f0 __attribute__((aligned(2))); };
union UAlC2 { long double f2 __attribute__((aligned(4))); char c[3]; };
struct S2p { unsigned f1; char f3[7]; unsigned b5 : 24; int b7 : 17; };
#pragma pack(push, 1)
struct PkUnionOuter { union { long double m0; struct S2p m1; } f3; struct { intptr_t m0; struct S2p m1; } f5; };
#pragma pack(pop)
#pragma pack(push, 4)
struct PragmaAlC { unsigned long long b2 : 43; uint64_t f3 __attribute__((aligned(2))); };
struct PragmaAlC2 { long long f4[6] __attribute__((aligned(2))); char f7; };
#pragma pack(pop)
/* `aligned` on a field of a packed struct is capped by an enclosing #pragma pack (found by fuzzing). */
#pragma pack(push, 4)
struct PkPragmaAl { char a; int b __attribute__((aligned(8))); } __attribute__((packed));
#pragma pack(pop)

/* `typedef struct tag {...} name_t;` used by its typedef name (tag differs). */
typedef struct TagRec { void *base; size_t size; } TagRec_t;
struct UsesTagTd { char a; TagRec_t r; int k; };
struct UsesTagUnion { union { TagRec_t os; TagRec_t arena; } mem; int kind; _Bool pinned; };

#define P(T,L) printf(#T " %zu %zu " #L "\n", sizeof(struct T), offsetof(struct T, L))
int main(){
P(Arr,c);P(ArrMac,c);P(ArrEnum,c);P(ArrExpr,c);P(ArrSizeof,c);P(Arr2D,c);P(ArrStruct,c);P(Enum,c);P(LD,c);P(I128,c);P(Cx,c);P(CxF,c);P(Wc,c);P(Bool,c);P(Sz,c);P(LL,d);P(FP,c);P(Pk,c);P(PkOuter,c);P(Al,c);P(AlSO,c);P(NestedPtr,c);P(NestInl,c);P(NestInlArr,c);P(WithU,c);P(WithUInl,c);P(Flex,data);P(Flex2,data);P(Bits,c);P(BitsMix,d);P(BitsL,t);P(Const,c);P(Unsigned,d);P(Ptrs,b);P(Atom,c);P(Align,c);P(F16,c);P(Typedef,c);P(TD,e);P(Arr3,d);P(ZeroArr,c);P(Pp,c);P(Dup,c);P(BfDiff,t);P(BfAnon,t);P(BfZero,t);P(BfWide,c);P(BfPk,d);P(PkUnionOuter,f5);P(PragmaAlC,f3);P(PragmaAlC2,f7);P(UsesFwd,c);P(MacMem,c);P(Vec,c);P(MacTy,c);P(PkPragmaAl,b);P(UsesTagTd,k);P(UsesTagUnion,kind);
printf("AlS %zu -1\nU %zu 0\nUAlC %zu -1\nUAlC2 %zu -1\n",sizeof(struct AlS),sizeof(union U),sizeof(union UAlC),sizeof(union UAlC2));
}
