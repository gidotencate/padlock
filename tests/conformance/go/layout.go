package main

import (
	"database/sql/driver"
	"fmt"
	"io"
	"unsafe"
	"sync"
	"sync/atomic"
	"time"
)

type Arr struct { a byte; b [48]byte; c int64 }
type Arr2 struct { a int8; b [3]int32; c int8 }
type Arr3 struct { a int8; b [2][3]int16; c int8 }
type ArrStr struct { a bool; b [2]string; c bool }
type Anon struct { a bool; _ [7]byte; c int64 }
type Emb struct { a bool; sync.Mutex; b bool }
type Emb2 struct { a bool; *Arr; b bool }
type Strct struct { a bool; inner struct{ x bool; y int64 }; b bool }
type Fn struct { a bool; f func(int); g func() ; b bool }
type Unsafe struct { a bool; p unsafe.Pointer; b bool }
type Cplx struct { a bool; c complex64; b bool }
type Mul struct { a, b bool; c, d int32; e int8 }
type Ch struct { a bool; c <-chan int; d chan<- int; b bool }
type At struct { a bool; x atomic.Int32; y atomic.Pointer[int]; z atomic.Int64; b bool }
type Wg struct { a bool; w sync.WaitGroup; b bool }
type Tm struct { a bool; t time.Time; d time.Duration; b bool }
type Ifc struct { a bool; e error; f any; g interface{ M() }; b bool }
type Zero struct { a bool; z struct{}; b bool }
type Zero2 struct { a bool; z [0]int64; b bool }
type Rw struct { a bool; r sync.RWMutex; b bool }
type Once struct { a bool; o sync.Once; b bool }
type Pool struct { a bool; p sync.Pool; b bool }
type Cond struct { a bool; c sync.Cond; b bool }
type MapT struct { a bool; m sync.Map; b bool }
type Alias int16
type UA struct { a bool; x Alias; b bool }
type Const [4]int32
const (
	ca = iota
	cb
	cc
)
type Iota struct { x [cc + 1]byte; y int32 }
type Hot struct { a atomic.Int64; b atomic.Uint64; _ [64 - unsafe.Sizeof(atomic.Int64{}) - unsafe.Sizeof(atomic.Uint64{})]byte; mu sync.Mutex; c int64 }
type Inner struct { x bool; y int32 }
type StArr struct { a bool; b [2]Inner; c bool }
type ZeroTrail struct { a int64; z struct{} }
type Ifaces struct { c driver.Connector; r io.Reader; x bool }
type GPair[K comparable, V any] struct { k K; flag bool; v V }
type GUser struct { a bool; p GPair[int8, int64]; q GPair[string, bool]; z bool }
type (
	G1 struct { x int32; y bool }
	G2 struct { y int64; x bool }
)
type UC struct { a bool; x Const; b bool }

func main() {
	var h1 Iota; fmt.Println("Iota", unsafe.Sizeof(h1), unsafe.Offsetof(h1.y))
	var h2 Hot; fmt.Println("Hot", unsafe.Sizeof(h2), unsafe.Offsetof(h2.c))
	var h3 StArr; fmt.Println("StArr", unsafe.Sizeof(h3), unsafe.Offsetof(h3.c))
	var h4 ZeroTrail; fmt.Println("ZeroTrail", unsafe.Sizeof(h4), unsafe.Offsetof(h4.z))
	var h5 Ifaces; fmt.Println("Ifaces", unsafe.Sizeof(h5), unsafe.Offsetof(h5.x))
	var h6 G1; fmt.Println("G1", unsafe.Sizeof(h6), unsafe.Offsetof(h6.y))
	var g1 GUser; fmt.Println("GUser", unsafe.Sizeof(g1), unsafe.Offsetof(g1.z))
	var g2 GPair[int8, int64]; fmt.Println("GPair[int8, int64]", unsafe.Sizeof(g2), -1)
	var g3 GPair[string, bool]; fmt.Println("GPair[string, bool]", unsafe.Sizeof(g3), -1)
	var h7 G2; fmt.Println("G2", unsafe.Sizeof(h7), unsafe.Offsetof(h7.x))
	var x Arr; fmt.Println("Arr", unsafe.Sizeof(x), unsafe.Offsetof(x.c))
	var x2 Arr2; fmt.Println("Arr2", unsafe.Sizeof(x2), unsafe.Offsetof(x2.c))
	var x3 Arr3; fmt.Println("Arr3", unsafe.Sizeof(x3), unsafe.Offsetof(x3.c))
	var x4 ArrStr; fmt.Println("ArrStr", unsafe.Sizeof(x4), unsafe.Offsetof(x4.c))
	var x5 Anon; fmt.Println("Anon", unsafe.Sizeof(x5), unsafe.Offsetof(x5.c))
	var x6 Emb; fmt.Println("Emb", unsafe.Sizeof(x6), unsafe.Offsetof(x6.b))
	var x7 Emb2; fmt.Println("Emb2", unsafe.Sizeof(x7), unsafe.Offsetof(x7.b))
	var x8 Strct; fmt.Println("Strct", unsafe.Sizeof(x8), unsafe.Offsetof(x8.b))
	var x9 Fn; fmt.Println("Fn", unsafe.Sizeof(x9), unsafe.Offsetof(x9.b))
	var x10 Unsafe; fmt.Println("Unsafe", unsafe.Sizeof(x10), unsafe.Offsetof(x10.b))
	var x11 Cplx; fmt.Println("Cplx", unsafe.Sizeof(x11), unsafe.Offsetof(x11.b))
	var x12 Mul; fmt.Println("Mul", unsafe.Sizeof(x12), unsafe.Offsetof(x12.e))
	var x13 Ch; fmt.Println("Ch", unsafe.Sizeof(x13), unsafe.Offsetof(x13.b))
	var x14 At; fmt.Println("At", unsafe.Sizeof(x14), unsafe.Offsetof(x14.b))
	var x15 Wg; fmt.Println("Wg", unsafe.Sizeof(x15), unsafe.Offsetof(x15.b))
	var x16 Tm; fmt.Println("Tm", unsafe.Sizeof(x16), unsafe.Offsetof(x16.b))
	var x17 Ifc; fmt.Println("Ifc", unsafe.Sizeof(x17), unsafe.Offsetof(x17.b))
	var x18 Zero; fmt.Println("Zero", unsafe.Sizeof(x18), unsafe.Offsetof(x18.b))
	var x19 Zero2; fmt.Println("Zero2", unsafe.Sizeof(x19), unsafe.Offsetof(x19.b))
	var x20 Rw; fmt.Println("Rw", unsafe.Sizeof(x20), unsafe.Offsetof(x20.b))
	var x21 Once; fmt.Println("Once", unsafe.Sizeof(x21), unsafe.Offsetof(x21.b))
	var x22 Pool; fmt.Println("Pool", unsafe.Sizeof(x22), unsafe.Offsetof(x22.b))
	var x23 Cond; fmt.Println("Cond", unsafe.Sizeof(x23), unsafe.Offsetof(x23.b))
	var x24 MapT; fmt.Println("MapT", unsafe.Sizeof(x24), unsafe.Offsetof(x24.b))
	var x25 UA; fmt.Println("UA", unsafe.Sizeof(x25), unsafe.Offsetof(x25.b))
	var x26 UC; fmt.Println("UC", unsafe.Sizeof(x26), unsafe.Offsetof(x26.b))
}
