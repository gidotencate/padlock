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
#define P(T,L) printf(#T " %zu %zu\n", sizeof(T), offsetof(T, L))
int main(){
P(Arr,c);P(ArrC,c);P(Ref,c);P(RRef,c);P(En,c);P(Str,c);P(Sp,c);P(At,c);P(Mx,c);P(Mp,c);P(Ar,c);P(Op,c);P(Fn,c);P(Der,c);P(Der2,c);P(Vb,c);P(VD,d);P(Mem,c);P(Fp,c);P(Nst,c);P(Ptr,c);P(Bf,c);P(St,b);P(Cx,c);P(Mut,c);
}
