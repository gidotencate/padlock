// Generate cpp_inheritance.pdb with MSVC:
//   cl /Zi /c cpp_inheritance.cpp
//   link /DEBUG /NOENTRY /DLL cpp_inheritance.obj /OUT:cpp_inheritance.dll
// Then copy cpp_inheritance.pdb to this directory and commit it.

struct Base { int x; double y; };          // 16B: x@0(4B) + 4pad + y@8(8B)
struct Derived : Base { int z; };          // 24B: [Base]@0(16B) + z@16(4B) + 4pad
struct Base2 { double c; };               // 8B
struct Multi : Base, Base2 { int d; };    // 32B: [Base]@0 + [Base2]@16 + d@24 + 4pad

Base  g_b;
Derived g_d;
Base2 g_b2;
Multi g_m;
