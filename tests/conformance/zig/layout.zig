const std = @import("std");
const N = 8;
const In = extern struct { x: u8, y: u32 };
const E8 = enum(u8) { a, b };
const E32 = enum(u32) { a, b };
const Arr = extern struct { a: u8, b: [48]u8, c: u64 };
const ArrConst = extern struct { a: u8, b: [N]u8, c: u64 };
const ArrExpr = extern struct { a: u8, b: [N * 2 + 1]u8, c: u64 };
const ArrArr = extern struct { a: u8, b: [2][3]u16, c: u8 };
const ArrS = extern struct { a: u8, b: [2]In, c: u8 };
const Ptrs = extern struct { a: u8, p: *u8, q: ?*u8, r: [*]u8, c: u8 };
const Slice = struct { a: u8, s: []u8, t: []const u8, c: u8 };
const Opt = struct { a: u8, o: ?u32, p: ?*u8, q: ?u8, c: u8 };
const Enums = extern struct { a: u8, e: E8, f: E32, c: u8 };
const Wide = struct { a: u8, x: u128, y: i128, f: f16, g: f32, h: f64, c: u8 };
const Odd = struct { a: u8, x: u24, y: i3, z: u65, c: u8 };
const Us = extern struct { a: u8, u: usize, i: isize, c: u8 };
const Cty = extern struct { a: u8, x: c_int, y: c_long, z: c_longlong, w: c_short, c: u8 };
const Fnp = extern struct { a: u8, f: *const fn (u8) callconv(.c) void, c: u8 };
const Nested = extern struct { a: u8, n: In, c: u8 };
const NestedAnon = struct { a: u8, n: struct { x: u8, y: u64 }, c: u8 };
const Un = extern union { a: u8, b: u64, c: [13]u8 };
const WithU = extern struct { a: u8, u: Un, c: u8 };
const Pk = packed struct { a: u8, b: u32, c: u16 };
const PkBits = packed struct { a: u3, b: u5, c: u8, d: bool };
const PkOuter = extern struct { a: u8, p: In, c: u8 };
const Al = extern struct { a: u8, b: u32 align(16), c: u8 };
const Bool = struct { a: u8, b: bool, c: u8 };
const Err = struct { a: u8, e: anyerror!u32, f: anyerror, c: u8 };
const Vec = struct { a: u8, v: @Vector(4, f32), c: u8 };
const TU = union(enum) { a: u8, b: u64 };
const WTU = struct { a: u8, t: TU, c: u8 };
const Str = struct { a: u8, s: []const u8, c: u8 };
const Void = struct { a: u8, v: void, c: u8 };
const Atom = struct { a: u8, x: std.atomic.Value(u32), y: std.atomic.Value(u64), c: u8 };

fn p(comptime T: type, comptime f: []const u8) void {
    std.debug.print("{s} {d} {d}\n", .{ @typeName(T)[std.mem.lastIndexOfScalar(u8, @typeName(T), '.').? + 1 ..], @sizeOf(T), @offsetOf(T, f) });
}

pub fn main() void {
    p(Arr, "c");
    p(ArrConst, "c");
    p(ArrExpr, "c");
    p(ArrArr, "c");
    p(ArrS, "c");
    p(Ptrs, "c");
    p(Enums, "c");
    p(Us, "c");
    p(Cty, "c");
    p(Fnp, "c");
    p(Nested, "c");
    p(WithU, "c");
    p(PkOuter, "c");
    p(Al, "c");
    std.debug.print("Un {d} 0\n", .{@sizeOf(Un)});
    std.debug.print("Pk {d} -1\n", .{@sizeOf(Pk)});
    std.debug.print("PkBits {d} -1\n", .{@sizeOf(PkBits)});
    std.debug.print("TU {d} -1\n", .{@sizeOf(TU)});
}
