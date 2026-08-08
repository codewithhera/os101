//! Boot self-test for the in-kernel TinyCC.

use super::{eval_i32, eval_i32_with_printf, seed_toolchain};
use crate::selftest::Report;

pub fn run() -> Report {
    let mut report = Report::new();
    let heap0 = crate::allocator::used();

    crate::serial_println!("tcc: self-test starting");

    match seed_toolchain() {
        Ok(n) => {
            report.check("toolchain seed ok", true);
            crate::serial_println!("tcc: seeded {} files", n);
        }
        Err(e) => {
            crate::serial_println!("tcc: seed failed: {}", e);
            report.check("toolchain seed ok", false);
            return report;
        }
    }

    match eval_i32("int test(void){ return 6*7+1; }\n", "test") {
        Ok(v) => report.check("arithmetic 6*7+1", v == 43),
        Err(e) => {
            crate::serial_println!("tcc: arithmetic: {}", e);
            report.check("arithmetic 6*7+1", false);
        }
    }

    match eval_i32(
        "int test(void){ int s=0; for(int i=1;i<=10;i++) s+=i; return s; }\n",
        "test",
    ) {
        Ok(v) => report.check("sum 1..10", v == 55),
        Err(e) => {
            crate::serial_println!("tcc: loop: {}", e);
            report.check("sum 1..10", false);
        }
    }

    match eval_i32(
        "struct P{int x; int y;}; int test(void){ struct P p; p.x=3; p.y=4; return p.x*p.x+p.y*p.y; }\n",
        "test",
    ) {
        Ok(v) => report.check("struct pythag", v == 25),
        Err(e) => {
            crate::serial_println!("tcc: struct: {}", e);
            report.check("struct pythag", false);
        }
    }

    match eval_i32(
        "static int add(int a,int b){return a+b;} int test(void){ int(*f)(int,int)=add; return f(20,22); }\n",
        "test",
    ) {
        Ok(v) => report.check("function pointer", v == 42),
        Err(e) => {
            crate::serial_println!("tcc: fnptr: {}", e);
            report.check("function pointer", false);
        }
    }

    match eval_i32(
        "int fib(int n){ return n<2?n:fib(n-1)+fib(n-2); } int test(void){ return fib(10); }\n",
        "test",
    ) {
        Ok(v) => report.check("fib(10)", v == 55),
        Err(e) => {
            crate::serial_println!("tcc: recursion: {}", e);
            report.check("fib(10)", false);
        }
    }

    match eval_i32_with_printf(
        "int putchar(int); int test(void){ const char*s=\"Hi\"; for(;*s;s++) putchar(*s); return 2; }\n",
        "test",
    ) {
        Ok((v, out)) => {
            report.check("putchar return", v == 2);
            report.check("putchar output", out == "Hi");
        }
        Err(e) => {
            crate::serial_println!("tcc: putchar: {}", e);
            report.check("putchar return", false);
        }
    }

    match eval_i32("int test(void){ return 1 + ; }\n", "test") {
        Ok(_) => report.check("syntax error rejected", false),
        Err(e) => {
            report.check("syntax error rejected", true);
            report.check("syntax diagnostic nonempty", !e.is_empty());
            crate::serial_println!("tcc: syntax diag: {}", e);
        }
    }

    let t0 = crate::clock::ticks();
    let heap_before = crate::allocator::used();
    let timed = eval_i32("int test(void){ return 123; }\n", "test");
    let t1 = crate::clock::ticks();
    let heap_after = crate::allocator::used();
    let ms = t1.saturating_sub(t0) * 1000 / 18;
    match timed {
        Ok(v) => report.check("timed compile ok", v == 123),
        Err(e) => {
            crate::serial_println!("tcc: timed: {}", e);
            report.check("timed compile ok", false);
        }
    }
    crate::serial_println!(
        "tcc: small compile ~{} ms, heap delta {} bytes (selftest heap {} -> {})",
        ms,
        heap_after.saturating_sub(heap_before),
        heap0,
        crate::allocator::used()
    );

    report
}
