#![allow(dead_code)]
use std::mem::{size_of, offset_of};
use std::sync::{Arc, Mutex, RwLock, atomic::*};
use std::collections::*;
use std::cell::*;
const N: usize = 8;
#[derive(Clone, Copy)] #[repr(C)] struct In { x: u8, y: u32 }
#[repr(C)] struct Arr { a: u8, b: [u8; 48], c: u64 }
#[repr(C)] struct ArrConst { a: u8, b: [u8; N], c: u64 }
#[repr(C)] struct ArrExpr { a: u8, b: [u8; N * 2 + 1], c: u64 }
#[repr(C)] struct ArrArr { a: u8, b: [[u16; 3]; 2], c: u8 }
#[repr(C)] struct ArrS { a: u8, b: [In; 2], c: u8 }
#[repr(C)] struct Tup { a: u8, t: (u8, u32, u8), c: u8 }
#[repr(C)] struct Unit { a: u8, t: (), c: u8 }
#[repr(C)] struct Str { a: u8, s: String, v: Vec<u8>, c: u8 }
#[repr(C)] struct Opt { a: u8, o: Option<u32>, p: Option<Box<u8>>, q: Option<u8>, c: u8 }
#[repr(C)] struct Atom { a: u8, x: AtomicU32, y: AtomicBool, z: AtomicU64, w: AtomicPtr<u8>, c: u8 }
#[repr(C)] struct Sync { a: u8, m: Mutex<u32>, r: RwLock<u8>, c: u8 }
#[repr(C)] struct Ptrs { a: u8, b: Box<u8>, r: Arc<u8>, s: &'static str, sl: &'static [u8], c: u8 }
#[repr(C)] struct Maps { a: u8, h: HashMap<u8,u8>, b: BTreeMap<u8,u8>, d: VecDeque<u8>, c: u8 }
#[repr(C)] struct Cells { a: u8, c1: Cell<u32>, r: RefCell<u8>, u: UnsafeCell<u64>, c: u8 }
#[repr(C)] struct Fns { a: u8, f: fn(u8), g: Box<dyn Fn()>, c: u8 }
#[repr(C)] struct Num { a: u8, x: u128, y: i128, f: f32, d: f64, ch: char, b: bool, c: u8 }
#[repr(C)] struct Us { a: u8, u: usize, i: isize, c: u8 }
#[repr(C, packed)] struct Pk { a: u8, b: u32, c: u16 }
#[repr(C, packed(2))] struct Pk2 { a: u8, b: u32, c: u8 }
#[repr(C, align(16))] struct Al { a: u8 }
#[repr(C)] struct AlO { a: u8, x: Al, c: u8 }
#[repr(C)] struct Nested { a: u8, n: In, c: u8 }
#[repr(C)] struct Pd { a: u8, p: std::marker::PhantomData<u64>, c: u8 }
#[repr(u8)] enum E8 { A, B }
#[repr(C)] enum EC { A, B }
#[repr(C)] struct WE { a: u8, e: E8, f: EC, c: u8 }
#[repr(C)] struct Dur { a: u8, d: std::time::Duration, i: std::time::Instant, c: u8 }
#[repr(C)] struct NZ { a: u8, n: std::num::NonZeroU32, c: u8 }
#[repr(C)] struct Raw { a: u8, p: *const u8, q: *mut In, c: u8 }
#[repr(C)] struct Wrap { a: u8, w: std::num::Wrapping<u16>, m: std::mem::ManuallyDrop<u32>, mu: std::mem::MaybeUninit<u64>, c: u8 }
#[repr(C)] struct Pin { a: u8, p: std::pin::Pin<Box<u8>>, c: u8 }
#[repr(C)] struct PS { a: u8, p: std::path::PathBuf, o: std::ffi::OsString, c: u8 }
#[repr(C)] struct Bs { a: u8, s: std::sync::mpsc::Sender<u8>, c: u8 }
#[repr(C)] struct Ty { a: u8, t: (u8, u8), c: u8 }
#[repr(C)] struct Rg { a: u8, r: std::ops::Range<u32>, c: u8 }
type Word = u16;
const LEN: usize = 3;
const BIG: usize = LEN * 2 + size_of::<u64>();
#[repr(C)] struct Opts { a: u8, o1: Option<&'static str>, o2: Option<Vec<u8>>, o3: Option<bool>, o4: Option<char>, o5: Option<Box<str>>, o6: Option<String>, o7: Option<std::time::Duration>, c: u8 }
#[repr(C)] struct Unsz { a: u8, b: Box<str>, c: Box<[u8]>, d: Arc<str>, e: &'static [u8], f: *const [u8], g: u8 }
#[repr(C)] struct Alias { a: u8, w: Word, x: [Word; LEN], c: u8 }
#[repr(C)] struct Lens { a: u8, b: [u8; BIG], c: [u32; LEN as usize], d: u8 }
#[repr(C)] struct NestArr { a: u8, n: [[In; 2]; 3], c: u8 }
#[repr(C, packed(4))] struct Pk4 { a: u8, b: u64, c: u8 }
#[repr(C)] struct SyncMore { a: u8, cv: std::sync::Condvar, o: std::sync::Once, ol: std::sync::OnceLock<u64>, c: u8 }
#[repr(C)] struct Ranges { a: u8, r: std::ops::RangeInclusive<u16>, s: std::ops::Range<u64>, c: u8 }
#[repr(u16)] enum E16 { A, B }
enum One { Only }
#[repr(C)] struct EnumF { a: u8, e: E16, o: One, c: u8 }
#[repr(C)] struct CStr { a: u8, s: std::ffi::CString, c: u8 }
#[repr(C)] struct TupNest { a: u8, t: ((u8, u16), u32, [u8; 3]), u: (In, u8), c: u8 }
#[repr(C, align(64))] struct AlNest { i: In, c: u8 }
#[repr(C, packed(2))] struct PkNest { a: u8, i: In, l: u64 }
// repr(Rust): rustc picks the order, so only sizes are compared (offset -1).
struct RA { a: u8, b: u64, c: u8 }
struct RB { a: u8, b: u16, c: u8, d: u32, e: u8 }
struct RC { a: bool, b: u64, c: Option<u32>, d: u8 }
struct RD { a: [u8; 3], b: u32, c: u16 }
struct RE { a: u8, b: (u8, u32), c: u16 }
struct RF { a: u8, b: Option<Box<u8>>, c: u8 }
struct RG { a: char, b: u8, c: u16 }
struct RH { a: u8, inner: RA, z: u8 }
#[repr(C)] struct RI { a: u8, inner: RB, z: u8 }
#[repr(C)] struct GPair<A, B> { a: A, b: B }
#[repr(C)] struct GBuf<T, const N: usize = 4> { head: u8, data: [T; N] }
#[repr(C)] struct GUser { x: u8, p: GPair<u8, u64>, b: GBuf<u16, 3>, d: GBuf<u32>, z: u8 }
type GP2 = GPair<u32, u8>;
// `keep::<T>()` holds a `MaybeUninit<T>` local so the debug build describes `T`
// (rustc emits debuginfo only for types that values have).
fn keep<T>() { let k = std::mem::MaybeUninit::<T>::uninit(); std::hint::black_box(&k); }
macro_rules! ps { ($($t:ident),*) => { $(keep::<$t>(); println!("{} {} -1", stringify!($t), size_of::<$t>());)* } }
macro_rules! p { ($t:ident, $f:ident) => { keep::<$t>(); println!("{} {} {} {}", stringify!($t), size_of::<$t>(), offset_of!($t, $f), stringify!($f)); } }
fn main() {
p!(Arr,c);p!(ArrConst,c);p!(ArrExpr,c);p!(ArrArr,c);p!(ArrS,c);p!(Tup,c);p!(Unit,c);p!(Str,c);p!(Opt,c);p!(Atom,c);p!(Sync,c);p!(Ptrs,c);p!(Maps,c);p!(Cells,c);p!(Fns,c);p!(Num,c);p!(Us,c);p!(Pk,c);p!(Pk2,c);p!(AlO,c);p!(Nested,c);p!(Pd,c);p!(WE,c);p!(Dur,c);p!(NZ,c);p!(Raw,c);p!(Wrap,c);p!(Pin,c);p!(PS,c);p!(Bs,c);p!(Ty,c);p!(Rg,c);
p!(Opts,c);p!(Unsz,g);p!(Alias,c);p!(Lens,d);p!(NestArr,c);p!(Pk4,c);p!(SyncMore,c);p!(Ranges,c);p!(EnumF,c);p!(CStr,c);p!(TupNest,c);p!(AlNest,c);ps!(RA, RB, RC, RD, RE, RF, RG, RH);p!(RI,z);p!(GUser,z);
    keep::<GPair<u8, u64>>(); keep::<GBuf<u16, 3>>(); keep::<GBuf<u32>>(); keep::<GP2>();
    println!("GPair<u8, u64> {} -1", size_of::<GPair<u8, u64>>());
    println!("GBuf<u16, 3> {} -1", size_of::<GBuf<u16, 3>>());
    println!("GBuf<u32> {} -1", size_of::<GBuf<u32>>());
    println!("GPair<u32, u8> {} -1", size_of::<GP2>());p!(PkNest,l);
    println!("Al {} 0", size_of::<Al>());
}
