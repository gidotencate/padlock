#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>
#include <memory>
#include <atomic>
#include <mutex>
#include <map>
#include <array>
#include <optional>
#include <functional>
constexpr int N = 8;
struct In { char x; int y; };
struct Arr { char a; char b[48]; long c; };
struct ArrC { char a; char b[N]; long c; };
struct Ref { char a; int& r; char c; };
struct RRef { char a; int&& r; char c; };
enum class EC : uint8_t { A, B };
enum class EI { A, B };
struct En { char a; EC e; EI f; char c; };
struct Str { char a; std::string s; std::vector<int> v; char c; };
struct Sp { char a; std::shared_ptr<int> s; std::unique_ptr<int> u; char c; };
struct At { char a; std::atomic<int> x; std::atomic<long> y; std::atomic<bool> z; char c; };
struct Mx { char a; std::mutex m; char c; };
struct Mp { char a; std::map<int,int> m; char c; };
struct Ar { char a; std::array<int,3> x; char c; };
struct Op { char a; std::optional<int> o; char c; };
struct Fn { char a; std::function<void()> f; char c; };
struct Base { int x; };
struct Der : Base { char c; };
struct Der2 : Base, In { char c; };
struct Vb { virtual void f(); char c; };
struct VD : Vb { char d; };
struct Mem { char a; void (In::*pm)(); int In::*pd; char c; };
struct Fp { char a; void (*f)(int); char c; };
struct Nst { char a; struct { char x; long y; } n; char c; };
struct Ptr { char a; In* p; char c; };
struct Bf { unsigned a:3; unsigned b:5; char c; };
struct St { static int s; char a; int b; };
struct Cx { char a; bool b; wchar_t w; char16_t c16; char32_t c32; char c; };
struct Mut { char a; mutable int m; const int k; char c; };
struct alignas(64) AlNest { In i; char c; };
#pragma pack(push, 2)
struct PkNest { char a; In i; long l; };
#pragma pack(pop)
struct BfNest { unsigned a:3; In i; char t; };
struct Pod { int a; char b; };
struct TP1 : Pod { char c; };
class Priv { int a; char b; public: int get(); };
struct TP2 : Priv { char c; };
struct Ctor { int a; char b; Ctor(); };
struct TP3 : Ctor { char c; };
struct Nsdmi { int a = 0; char b; };
struct TP4 : Nsdmi { char c; };
struct Dtor { int a; char b; ~Dtor(); };
struct TP5 : Dtor { char c; };
struct DefC { int a; char b; DefC() = default; };
struct TP6 : DefC { char c; };
struct Empty {};
struct EB1 : Empty { int x; };
struct EB2 : Empty { };
struct EB3 : Empty, Pod { char t; };
struct Nua { char a; [[no_unique_address]] Empty e; int x; };
struct VA { int a; };
struct VBb : virtual VA { int b; };
struct VCc : virtual VA { int c; };
struct VDd : VBb, VCc { int d; };
struct NonDyn { char z; };
struct PrimLater : NonDyn, Vb { char q; };
struct OnlyVirtualBase : virtual Empty { char k; };
template<typename T, int N = 4, class Tag = Empty> struct TBuf { char head; T data[N]; Tag tag; };
template<class T> struct TNode { char used; T v; TNode<T>* next; };
template<> struct TNode<char> { char used; char v; };
template struct TBuf<long>;
using TB2 = TBuf<short, 2>;
using u32_alias = unsigned int;
struct TUser { char a; TBuf<char, 8> b; TNode<double> n; TB2 t2; u32_alias u; TNode<char> nc; char z; };
#define P(T,L) printf(#T " %zu %zu " #L "\n", sizeof(T), offsetof(T, L))
int main(){
P(Arr,c);P(ArrC,c);P(Ref,c);P(RRef,c);P(En,c);P(Str,c);P(Sp,c);P(At,c);P(Mx,c);P(Mp,c);P(Ar,c);P(Op,c);P(Fn,c);P(Der,c);P(Der2,c);P(Vb,c);P(VD,d);P(Mem,c);P(Fp,c);P(Nst,c);P(Ptr,c);P(Bf,c);P(St,b);P(Cx,c);P(Mut,c);P(AlNest,c);P(PkNest,l);P(BfNest,t);P(TP1,c);P(TP2,c);P(TP3,c);P(TP4,c);P(TP5,c);P(TP6,c);P(EB1,x);P(EB3,t);P(Nua,x);P(VDd,d);P(PrimLater,q);P(OnlyVirtualBase,k);P(TUser,z);
printf("TBuf<long> %zu -1\nTBuf<char, 8> %zu -1\nTNode<double> %zu -1\nTNode<char> %zu -1\nTBuf<short, 2> %zu -1\n", sizeof(TBuf<long>), sizeof(TBuf<char, 8>), sizeof(TNode<double>), sizeof(TNode<char>), sizeof(TBuf<short, 2>));
printf("EB2 %zu -1\nVBb %zu -1\n", sizeof(EB2), sizeof(VBb));
}
