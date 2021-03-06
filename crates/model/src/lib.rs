//! simple model-checking tools for rust.
//!
//! aims to reduce the boiler plate required
//! to write model-based and linearizability tests.
//!
//! it can find linearizability violations in
//! implementations without having any knowledge
//! of their inner-workings, by running sequential
//! operations on different commands and then
//! trying to find a sequential ordering that results
//! in the same return values.
//!
//! model-based testing:
//!
//! ```rust,noexecute
//! #[macro_use]
//! extern crate model;
//! #[macro_use]
//! extern crate proptest;
//!
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! model! {
//!     Model => let m = AtomicUsize::new(0),
//!     Implementation => let mut i: usize = 0,
//!     Add(usize)(v in 0usize..4) => {
//!         let expected = m.fetch_add(v, Ordering::SeqCst) + v;
//!         i += v;
//!         assert_eq!(expected, i);
//!     },
//!     Set(usize)(v in 0usize..4) => {
//!         m.store(v, Ordering::SeqCst);
//!         i = v;
//!     },
//!     Eq(usize)(v in 0usize..4) => {
//!         let expected = m.load(Ordering::SeqCst) == v;
//!         let actual = i == v;
//!         assert_eq!(expected, actual);
//!     },
//!     Cas((usize, usize))((old, new) in (0usize..4, 0usize..4)) => {
//!         let expected =
//!             m.compare_and_swap(old, new, Ordering::SeqCst);
//!         let actual = if i == old {
//!             i = new;
//!             old
//!         } else {
//!             i
//!         };
//!         assert_eq!(expected, actual);
//!     }
//! }
//! ```
//!
//! linearizability testing:
//!
//! ```rust,noexecute
//! #[macro_use]
//! extern crate model;
//! #[macro_use]
//! extern crate proptest;
//!
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! linearizable! {
//!     Implementation => let i = Shared::new(AtomicUsize::new(0)),
//!     BuggyAdd(usize)(v in 0usize..4) -> usize {
//!         let current = i.load(Ordering::SeqCst);
//!         thread::yield_now();
//!         i.store(current + v, Ordering::SeqCst);
//!         current + v
//!     },
//!     Set(usize)(v in 0usize..4) -> () {
//!         i.store(v, Ordering::SeqCst)
//!     },
//!     BuggyCas((usize, usize))((old, new) in (0usize..4, 0usize..4)) -> usize {
//!         let current = i.load(Ordering::SeqCst);
//!         thread::yield_now();
//!         if current == old {
//!             i.store(new, Ordering::SeqCst);
//!             new
//!         } else {
//!             current
//!         }
//!     }
//! }
//! ```
extern crate permutohedron;

use std::ops::Deref;

pub struct Shared<T>(*mut T);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Shared<T> {
        *self
    }
}

impl<T> Copy for Shared<T> {}

unsafe impl<T> Sync for Shared<T> {}

unsafe impl<T> Send for Shared<T> {}

impl<T> Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0 }
    }
}

impl<T> Shared<T> {
    pub fn new(inner: T) -> Shared<T> {
        Shared(Box::into_raw(Box::new(inner)))
    }
}

#[macro_export]
macro_rules! model {
    (
        Model => $model:stmt,
        Implementation => $implementation:stmt,
        $($op:ident ($($type:ty),*) ($parm:pat in $strategy:expr) => $body:expr),*
    ) => {
        model! {
            Config => Config::with_cases(1000).clone_with_source_file(file!()),
            Model => $model,
            Implementation => $implementation,
            $($op ($($type),*) ($parm in $strategy) => $body),*
        }
    };
    (
        Config => $config:expr,
        Model => $model:stmt,
        Implementation => $implementation:stmt,
        $($op:ident ($($type:ty),*) ($parm:pat in $strategy:expr) => $body:expr),*
    ) => {
        use proptest::collection::vec as prop_vec;
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};

        #[derive(Debug)]
        enum Op {
            $(
                $op($($type),*)
            ),*
        }

        fn arb() -> BoxedStrategy<Vec<Op>> {
            prop_vec(
                prop_oneof![
                    $(
                        $strategy.prop_map(Op::$op)
                    ),*
                ],
                0..40,
            ).boxed()
        }

        let config = $config;
        let mut runner = TestRunner::new(config);

        match runner.run(&arb(), |ops| {
            $model;
            $implementation;

            for op in ops {
                match *op {
                    $(Op::$op($parm) => $body),*
                };
            };
            Ok(())
        }) {
            Ok(_) => {}
            Err(e) =>  panic!("{}\n{}", e, runner),
        }
    }
}

#[macro_export]
macro_rules! linearizable {
    (
        Implementation => $implementation:stmt,
        $($op:ident ($($type:ty),*) ($parm:pat in $strategy:expr) -> $ret:ty $body:block),*
    ) => {
        use std::thread;

        use proptest::collection::vec as prop_vec;
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};

        #[derive(Debug, Clone)]
        enum Op {
            $(
                $op($($type),*)
            ),*
        }

        #[derive(Debug, PartialEq)]
        enum Ret {
            $(
                $op($ret)
            ),*
        }

        fn arb() -> BoxedStrategy<(usize, Vec<Op>)> {
            prop_vec(
                prop_oneof![
                    $(
                        $strategy.prop_map(Op::$op)
                    ),*
                ],
                1..4,
            )
                .prop_flat_map(|ops| (0..ops.len(), Just(ops)))
                .boxed()
        }

        let config = Config::with_cases(10000).clone_with_source_file(file!());
        let mut runner = TestRunner::new(config);

        match runner.run(&arb(), |&(split_point, ref ops)| {
            $implementation;

            let ops1 = ops[0..split_point].to_vec();
            let ops2 = ops[split_point..].to_vec();

            let t1 = thread::spawn(move || {
                    let mut ret = vec![];
                    for op in ops1 {
                        ret.push(match op {
                            $(
                                Op::$op($parm) => Ret::$op($body)
                            ),*
                        });
                    }
                    ret
                });

            let t2 = thread::spawn(move || {
                    let mut ret = vec![];
                    for op in ops2 {
                        ret.push(match op {
                            $(
                                Op::$op($parm) => Ret::$op($body)
                            ),*
                        });
                    }
                    ret
                });

            let r1 = t1.join().expect("thread should not panic");
            let r2 = t2.join().expect("thread should not panic");

            // try to find sequential walk through ops
            let o1 = ops[0..split_point].to_vec();
            let o2 = ops[split_point..].to_vec();

            let calls1: Vec<(Op, Ret)> = o1.into_iter().zip(r1.into_iter()).collect();
            let calls2: Vec<(Op, Ret)> = o2.into_iter().zip(r2.into_iter()).collect();
            let mut indexes = vec![0; calls1.len()];
            indexes.resize(calls1.len() + calls2.len(), 1);
            let calls = vec![calls1, calls2];

            let mut linearizable = false;

            let call_permutations = model::permutohedron_heap(&mut indexes);

            'outer: for walk in call_permutations {
                $implementation;

                let mut indexes = vec![0, 0];
                // println!("trying walk {:?}", walk);

                for idx in walk {
                    let (ref op, ref expected) = calls[idx][indexes[idx]];
                    indexes[idx] += 1;

                    match *op {
                        $(
                            Op::$op($parm) => {
                                let ret = Ret::$op($body);
                                if ret != *expected {
                                    continue 'outer;
                                }
                            }
                        ),*
                    }
                }

                linearizable = true;
                break;
            }

            assert!(linearizable, "operations are not linearizable: {:?}", calls);

            Ok(())
        }) {
            Ok(_) => {}
            Err(e) =>  panic!("{}\n{}", e, runner),
        }
    }
}

pub fn permutohedron_heap<'a, Data, T>(
    orig: &'a mut Data,
) -> permutohedron::Heap<'a, Data, T>
    where Data: 'a + Sized + AsMut<[T]>,
          T: 'a
{
    permutohedron::Heap::new(orig)
}
